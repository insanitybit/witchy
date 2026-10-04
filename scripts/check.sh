#!/usr/bin/env bash
# The green gate: one command that checks the whole workspace, locally. Run it
# before you commit, and before you push. Steps are ordered cheap-to-expensive so
# a failure surfaces as early as possible.
#
#   ./scripts/check.sh --fast  the COMMIT gate: tests (minus the
#                              nightly-deferred perf/build tests, see
#                              NIGHTLY-DEFERRED below), plus witchy-fmt IF any
#                              .witchy files changed — the fast inner loop
#   ./scripts/check.sh         the MERGE gate: tests + fmt. Nextest compiles the
#                              selected production and test targets, so there is
#                              no duplicate cargo-check graph
#   ./scripts/check.sh --full  the PUSH gate: also the nightly-deferred tests and bench trend
#
# Shards — ONE named section, for a focused pre-queue run in your own worktree.
# A green shard does not replace the full gate; scripts/merge-queue.sh runs that
# once, serialized, at merge time:
#   ./scripts/check.sh --examples  just the example differential matrix (example_tests::*)
#   ./scripts/check.sh --queue-infra-edit  representative queue edit probes
#   ./scripts/check.sh --queue-infra  merge-queue fixtures, isolated and serial
# glamour, coven, coven-web, grimoire, the browser playground and the book/docs
# site are consumers of the compiler. This repo's gate and CI do not test them.
#
# rustfmt is deliberately NOT part of the gate: the Rust in this repo is
# hand-formatted, so `cargo fmt` would fight the intended style.
#
# DO NOT pipe this script through `tail`/`head`/`grep` (e.g. `check.sh | tail`): a
# pipeline's exit status is its LAST command's, so under a parent shell without
# `pipefail` the gate's own non-zero exit is MASKED — a red run then looks green
# (RFC-0058 §4). Run it bare, or redirect to a file and read that:
# `./scripts/check.sh >check.log 2>&1; tail check.log`. (This script sets `pipefail`
# for its OWN internal pipes; the hazard is a caller's pipe, which it cannot control.)
set -euo pipefail
cd "$(dirname "$0")/.."
# One feature resolution for the gate and agents.
# shellcheck source=feature-unification.sh
. scripts/feature-unification.sh

# The merge coordinator marks serialized gates with WITCHY_GATE_SCOPE. Clear
# Cargo's configured wrapper there so a detached coordinator does not inherit
# a sandbox-incompatible sccache process. The coordinator also exports these
# values after this change lands; keeping the check self-hosting lets this
# candidate pass when an older coordinator is the one gating it.
if [ -n "${WITCHY_GATE_SCOPE+x}" ]; then
    export RUSTC_WRAPPER=
    export CARGO_BUILD_RUSTC_WRAPPER=
    # Integration tests already require Cargo's normal `witchy` executable.
    # Embed the candidate identity in that build so the later Rust-class leg
    # can authenticate and reuse it instead of compiling a second feature
    # graph solely to add the commit string.
    export WITCHY_BUILD_COMMIT="$(git rev-parse --verify HEAD)"
    # nextest discovers tests by executing every test binary. macOS otherwise
    # pages large local-symbol tables into memory before any test runs. The
    # workspace Cargo.toml now strips `dev` and `test` for everyone (so every
    # build flow shares one set of artifacts); this export stays so an older
    # checkout gated by this coordinator still matches the prewarm, and an
    # explicit Cargo profile override remains authoritative.
    if [ -z "${CARGO_PROFILE_TEST_STRIP+x}" ]; then
        export CARGO_PROFILE_TEST_STRIP=symbols
    fi
    # Incremental is the serialized-gate profile (run_gate / prewarm / prepare
    # now match). `unset` still overrides a stale coordinator that exported 0.
    # Measured (test profile, 1-line change): incremental rebuild 13s vs
    # CARGO_INCREMENTAL=0 rebuild 72s. (Cost: incremental/ grows on disk.)
    unset CARGO_INCREMENTAL
    # The machine-wide ~/.cargo/config.toml caps every build at `jobs = 2` so
    # a dozen concurrent agents do not oversubscribe the cores. The serialized
    # gate is the one build everyone waits on, and under that cap its cold
    # compiles ran strictly two crates at a time: the RFC-0170 P2+P3 gate spent
    # 20m53s compiling. The gate takes eight build jobs, matching its eight
    # test jobs, while agents keep the cap. `WITCHY_GATE_BUILD_JOBS` retunes it.
    export CARGO_BUILD_JOBS="${WITCHY_GATE_BUILD_JOBS:-8}"
fi

# The test graph is the gate's critical path.
# macOS test discovery in the serialized gate is bounded to two cold binaries
# at a time by default in scripts/nextest-list-wrapper.sh (.config/nextest.toml),
# which can take longer than the three-pulse standalone startup window. Keep
# the heartbeat bounded while extending the serialized gate's observable-
# progress window to sixteen minutes.
if [ -n "${WITCHY_GATE_SCOPE+x}" ] && [ -z "${WITCHY_STAGE_HEARTBEAT_LIMIT+x}" ]; then
    export WITCHY_STAGE_HEARTBEAT_LIMIT=8
fi

# Test discovery is bounded separately by scripts/nextest-list-wrapper.sh.
# Cold, subprocess-heavy tests previously exceeded nextest's 135s limit at
# eight-job execution. They now stay in nextest's four-wide gate-cold-heavy
# group, while three consecutive exact-master runs proved that eight global
# jobs keep the rest of the suite below five minutes. Retain the explicit
# override for controlled retuning.
gate_test_jobs="${WITCHY_GATE_TEST_JOBS:-}"
if [ -n "${WITCHY_GATE_SCOPE+x}" ] && [ -z "$gate_test_jobs" ]; then
    # The eight-job default is the proven serialized-gate tuning (see comment
    # above) and is asserted by the merge-queue gate-behaviour test on every OS.
    # It was previously gated to Darwin only, which dropped `-j 8` on Linux CI —
    # both diverging from the contract and losing the tuning. Apply it whenever
    # the serialized gate runs, regardless of platform. `WITCHY_GATE_TEST_JOBS`
    # still overrides for controlled retuning.
    gate_test_jobs=8
fi
case "$gate_test_jobs" in
    "") ;;
    *[!0-9]*) echo "check.sh: WITCHY_GATE_TEST_JOBS must be a positive integer" >&2; exit 2 ;;
    *) [ "$gate_test_jobs" -gt 0 ] || { echo "check.sh: WITCHY_GATE_TEST_JOBS must be a positive integer" >&2; exit 2; } ;;
esac

full=0
fast=0
shard=""
for arg in "$@"; do
    case "$arg" in
        --full) full=1 ;;
        --fast) fast=1 ;;
        --examples | --queue-infra-edit | --queue-infra) shard="${arg#--}" ;;
        -h | --help) sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "check.sh: unknown argument '$arg' (try --fast, --full, --examples, --queue-infra-edit, --queue-infra, or --help)" >&2; exit 2 ;;
    esac
done
if [ -n "$shard" ] && { [ "$full" -eq 1 ] || [ "$fast" -eq 1 ]; }; then
    echo "check.sh: --$shard cannot be combined with --fast/--full" >&2; exit 2
fi

# Honor a custom CARGO_TARGET_DIR (concurrent agents run with per-agent target
# dirs, e.g. CARGO_TARGET_DIR=target-claude): the cargo build below writes the
# binary there, not under ./target, so the fmt check must look there too.
target_dir="${CARGO_TARGET_DIR:-target}"

# NIGHTLY-DEFERRED PERF/BUILD/COMPILE-BOUND TESTS ----------------------------
# scripts/gate-test-cost.py --runs 50 found every test costing
# more than 2s that ran in essentially every gate (n=40-41 of 50 mined logs).
# None gets to keep charging every merge just because it is correctness-
# adjudicating; the policy is optimize, replace, remove, or defer -- there is
# no fifth option. Two shapes account for all 17:
#
#   1. Genuine perf benchmarks / real subprocess builds (4 tests, 5-31s): no
#      smaller N preserves what they measure. rfc0129_scalar_executor_performance
#      ::million_message_scalar_executor_meets_resumption_cost_and_allocation_gate
#      and ::packed_aggregate_channel_uses_direct_scalar_lanes measure real
#      execution cost; rfc0092's trusted_publisher_application_end_to_end and
#      trusted_minigrep_distribution::committed_minigrep_builds_and_runs_as_an_
#      installed_trusted_app each shell out to a real `--release` cargo build,
#      the same shape as other real-build tests this gate defers.
#
#   2. RFC-0135 parity oracles and structural/tail-lowering checks over TINY
#      fixtures (13 tests, 2-26s) whose cost is the compiler's own optimizer
#      pipeline (`mode opt`, `OptSet::all()`, `optimized_wir`) invoked once per
#      test -- not test design, and not a magnitude a source-level rewrite can
#      fix. Read each one before adding it here: none scales a loop count or
#      message count that a smaller value would shrink without weakening the
#      assertion (see git blame on this block for the per-test audit). RFC-0135
#      already scopes "stays in the inner loop" to focused admitted-Wasm
#      <file>` runs on the tests a developer is touching, not to every fixture
#      re-running on every unrelated merge; the full corpus is explicitly
#      stabilization/release/periodic-CI, not every gate. Real fix is a shared
#      one -- cutting the compiler's optimizer-invocation overhead once so
#      every one of these gets cheap together -- tracked as a follow-up, not
#      attempted here.
#
# Deferred out of the default/--fast merge-gate; still run under --full, CI's
# per-push `cargo nextest run --workspace`, and explicitly via
# `cargo nextest run -E "$nightly_excl_positive"`.
#
# The list itself lives in scripts/nightly-deferred-tests.txt -- ONE source of
# truth shared with scripts/gate-test-cost.py --assert-budget, which exempts
# exactly these names when --full/CI enforces the 2s budget on everything
# else. Editing this list means editing that file, not this script.
nightly_deferred_file="scripts/nightly-deferred-tests.txt"
nightly_names=""
while IFS= read -r nightly_line || [ -n "$nightly_line" ]; do
    case "$nightly_line" in
        ''|'#'*) continue ;;
    esac
    nightly_names="${nightly_names:+$nightly_names|}^${nightly_line}\$"
done < "$nightly_deferred_file"
nightly_excl_positive="test(/${nightly_names}/)"
nightly_excl="not ($nightly_excl_positive)"

# Gate scope (WITCHY_GATE_SCOPE, default `all`) — set by the merge-queue
# coordinator from the batch diff (see merge-queue.sh, GATE_NO_TEST_PATHS and
# COST_TOOL_PATHS). An exact cost-report-only diff runs its Python regression.
# `docs` means every changed path is documentation or an agent-instruction
# file that NO test or gate stage reads: CLAUDE.md, the .claude/ and .codex/
# trees (skills, settings, hooks), and the documentation set (rfcs/ minus
# rfcs/0167-witchy-language-contract.md — which example_tests::
# public_sources_do_not_call_legacy_render_intrinsic reads — wiki/, bugs/,
# docs/ (agent playbook; example_tests walks README/CONTRIBUTING/spec/book
# only), and the gitignored scratch//security-eval/):
# such a diff cannot change any gate stage's outcome, so the heavy stages
# would only re-validate the already-gated master tree. The default merge-gate
# mode skips them; post-merge CI still runs the complete suite as the
# backstop. --fast, --full, and the shards ignore the scope entirely (force
# `all`): they are human/agent-invoked runs whose point is to execute their
# section.
gate_scope="${WITCHY_GATE_SCOPE:-all}"
case "$gate_scope" in
    all | docs | cost-tool) ;;
    *) echo "check.sh: unknown WITCHY_GATE_SCOPE='$gate_scope' (want all|docs|cost-tool)" >&2; exit 2 ;;
esac
if [ "$full" -eq 1 ] || [ "$fast" -eq 1 ]; then gate_scope="all"; fi

# CORPUS SWEEPS -------------------------------------------------------------
# The corpus sweeps walk every example, every std module and every ```witchy
# block in the docs, compiling and running each on both backends — roughly 900
# program compile+run cycles, the largest single block of work left in the
# suite. They are the differential corpus that guards the prime directive, so
# they are NOT keyed on whether examples/ changed: a compiler change with an
# untouched corpus is exactly the case they exist to catch.
#
# They are keyed on their INPUTS instead. A sweep can only produce a different
# answer if the diff touches something it reads: the compiler (crates/, src/,
# std/, build.rs, Cargo metadata, the toolchain), the corpus itself
# (examples/), or the prose corpora (book/, spec/, README.md). A candidate
# that touches none of those — a tests/ change, a scripts/ change, a CI or
# agent-config change — cannot move the result, so running it re-derives a
# known answer at full price. Measured over 200 commits: 22% of the ones that
# run a full gate today are in that set.
#
# merge-queue.sh classifies from the batch diff and fails SAFE to running
# them. Keep this filterset and that path list in step; when in doubt, add the
# path there rather than narrowing this expression.
sweeps_filter='test(/^example_tests::example_sweeps::/) or test(/^example_tests::region::examples_agree_under_inplace_and_forced_copy$/)'
gate_skip_sweeps="${WITCHY_GATE_SKIP_SWEEPS:-0}"
case "$gate_skip_sweeps" in
    0 | 1) ;;
    *) echo "check.sh: WITCHY_GATE_SKIP_SWEEPS must be 0 or 1" >&2; exit 2 ;;
esac
[ "$full" -eq 1 ] && gate_skip_sweeps=0
sweeps_excl=""
[ "$gate_skip_sweeps" -eq 1 ] && sweeps_excl="not ($sweeps_filter)"

# Serialized-gate skips for legs the batch cannot change. Coordinator sets
# these; --full and standalone keep the legs. 0/unset runs them.
gate_skip_rust_class="${WITCHY_GATE_SKIP_RUST_CLASS:-0}"
gate_skip_fmt="${WITCHY_GATE_SKIP_FMT:-0}"
case "$gate_skip_rust_class" in
    0 | 1) ;;
    *) echo "check.sh: WITCHY_GATE_SKIP_RUST_CLASS must be 0 or 1" >&2; exit 2 ;;
esac
case "$gate_skip_fmt" in
    0 | 1) ;;
    *) echo "check.sh: WITCHY_GATE_SKIP_FMT must be 0 or 1" >&2; exit 2 ;;
esac
[ "$full" -eq 1 ] && { gate_skip_rust_class=0; gate_skip_fmt=0; }
# Nested check.sh (queue-infra fixtures, an agent's local shard) must not
# inherit skip flags from a parent serialized gate. Only the coordinator's
# marked run (WITCHY_GATE_SCOPE set) applies them.
if [ -z "${WITCHY_GATE_SCOPE+x}" ]; then
    gate_skip_rust_class=0
    gate_skip_fmt=0
fi

# DEFERRED-FEATURE TESTS (RFC-0165) -------------------------------------------
# The canonical SafeWIR production route refuses every program outside its
# compact subset instead of falling back to the raw encoder, so the tests that
# compile programs from the deferred language families (tasks, capabilities
# beyond Console, Bytes, recursive types, generators, optimizer statistics,
# fixture flows over such programs) are red by policy, not by defect. They are
# preserved, listed one per line in scripts/rfc0165-deferred-tests.txt, and
# excluded from the blocking gate by exact binary id and test name; nothing
# else is excluded. The filterset is produced by ONE script,
# scripts/rfc0165-deferred-filter.sh, which CI consumes too, so the gate and CI
# cannot drift; it also refuses to defer the native security suites. `--full`
# and an explicitly EMPTY WITCHY_GATE_DEFERRED run the deferred tests. The
# list is reviewed in rfcs/0165-acceptance-ledger.md; a test leaves it when
# its family is admitted, never to hide a defect in an admitted one.
gate_deferred="${WITCHY_GATE_DEFERRED-scripts/rfc0165-deferred-tests.txt}"
[ "$full" -eq 1 ] && gate_deferred=""
deferred_excl=""
if [ -n "$gate_deferred" ]; then
    deferred_excl="$(./scripts/rfc0165-deferred-filter.sh "$gate_deferred")" || exit 2
    [ "$deferred_excl" = "all()" ] && deferred_excl=""
fi

# Queue fixtures spawn detached coordinators, process groups, lock holders, and
# nested throwaway Git repositories. Scheduling them inside the product run
# would put them beside rustc and at the product run's width, so they run as
# their own bounded-width nextest shard. The coordinator enables it only when the batch
# touches queue infrastructure; operators can force it for exact-master
# reliability baselines. It never runs beside the COMPILER: it starts only once
# the product nextest run announces itself (its Cargo build is finished) and
# reuses the already-built test binary. It then overlaps the product test run
# rather than following it: sequentially, the gate idled through the long tail
# of each leg (2026-09-26: 112s product run, then 132s of fixtures at a mean
# width of 5.6). Its output is captured and replayed in its own stage, so the
# log still reads leg by leg. WITCHY_GATE_QUEUE_INFRA_OVERLAP=0 restores the
# strictly sequential shard for reliability baselines.
queue_infra="${WITCHY_GATE_QUEUE_INFRA:-0}"
queue_infra_jobs="${WITCHY_QUEUE_INFRA_JOBS:-6}"
queue_infra_overlap="${WITCHY_GATE_QUEUE_INFRA_OVERLAP:-1}"
case "$queue_infra" in
    0 | 1) ;;
    *) echo "check.sh: WITCHY_GATE_QUEUE_INFRA must be 0 or 1" >&2; exit 2 ;;
esac
case "$queue_infra_overlap" in
    0 | 1) ;;
    *) echo "check.sh: WITCHY_GATE_QUEUE_INFRA_OVERLAP must be 0 or 1" >&2; exit 2 ;;
esac
case "$queue_infra_jobs" in
    '' | *[!0-9]* | 0)
        echo "check.sh: WITCHY_QUEUE_INFRA_JOBS must be a positive integer" >&2
        exit 2
        ;;
esac
[ "$queue_infra" -eq 0 ] || gate_scope="all"

# Prefer nextest (the project's runner); fall back to plain `cargo test`.
if cargo nextest --version >/dev/null 2>&1; then
    # On macOS the list wrapper avoids a second cold exec per test binary by
    # deriving the one audited legacy ignored test from the ordinary list. Keep that
    # optimization fail-closed: a new/renamed #[ignore] must update its policy first.
    scripts/nextest-list-wrapper.sh --validate-ignore-policy "$PWD"
    # Combine the exclusions into one filterset (nextest ANDs, so join with ` and `).
    # Queue-infrastructure fixtures run only in their isolated shard below.
    excl="not package(merge-queue-tests)"
    census_proof_sha="${WITCHY_GATE_CENSUS_PROOF_SHA:-}"
    if [ -n "$census_proof_sha" ] \
        && [ "$(git rev-parse HEAD 2>/dev/null || true)" = "$census_proof_sha" ]; then
        excl="$excl and not test(/^rfc0087_migration_census::repository_census_matches_the_checked_in_type_resolved_snapshot$/)"
        printf 'check.sh: reusing exact RFC-0087 census proof for %.12s\n' "$census_proof_sha"
    fi
    [ "$full" -eq 0 ] && excl="$excl and $nightly_excl"
    if [ -n "$deferred_excl" ]; then
        excl="${excl:+$excl and }$deferred_excl"
    fi
    if [ -n "$sweeps_excl" ]; then
        excl="${excl:+$excl and }$sweeps_excl"
    fi
    # In the SERIALIZED gate, stop at the first failure. The profile's
    # `fail-fast = false` is right for a developer, who wants every failure from
    # one local run — but the merge gate is not debugging, it is adjudicating,
    # and a candidate with one failing test is already rejected.
    #
    # The decisive fact is that the coordinator's culprit eviction consumes
    # exactly ONE failing target (the `reason` field of a red/batch_red journal
    # entry) and evicts exactly one branch. Every test run after the first
    # failure therefore produces information the queue never reads.
    #
    # Measured over 60 red gates: the first failure lands 22% of the way through
    # the suite (median), and 3.0h of the 4.5h those gates spent executing came
    # after it — 67% waste, a median of 179s per red gate. Raising the threshold
    # does not pay: a third of red gates have exactly one distinct failing test,
    # so `--max-fail=2` already forfeits them, and eviction would ignore the
    # extra targets anyway.
    #
    # `:immediate` does not wait out tests already in flight — a slow test that
    # started before the failure would otherwise hold the run open.
    #
    # Outside the gate this is `all`, which is exactly the profile's
    # `fail-fast = false`. Spelling the non-gate case as a real value rather
    # than an empty array is deliberate: macOS ships bash 3.2, where expanding
    # an empty array under `set -u` is an unbound-variable error.
    if [ -n "${WITCHY_GATE_SCOPE+x}" ]; then
        max_fail=--max-fail=1:immediate
    else
        max_fail=--max-fail=all
    fi
    # Serialized-gate path filter from the coordinator (test-for-paths.sh
    # crate/binary mapping). Unset, empty, WORKSPACE, --full, or a
    # non-serialized run keeps unfiltered --workspace. A focused selection
    # replaces --workspace with the mapped -p/--test args and ANDs any extra
    # inclusion expression with the existing exclusions.
    gate_nextest="${WITCHY_GATE_NEXTEST:-}"
    gate_nextest_expr="${WITCHY_GATE_NEXTEST_EXPR:-}"
    gate_nextest_extra="${WITCHY_GATE_NEXTEST_EXTRA:-}"
    queue_nextest_args=""
    [ "$full" -eq 1 ] && { gate_nextest=""; gate_nextest_expr=""; gate_nextest_extra=""; }
    if [ -z "${WITCHY_GATE_SCOPE+x}" ]; then
        gate_nextest=""
        gate_nextest_expr=""
        gate_nextest_extra=""
    fi
    case "$gate_nextest" in
        '' | WORKSPACE) gate_nextest="" ;;
        -p\ * | --test\ *) ;;
        *)
            echo "check.sh: ignoring unusable WITCHY_GATE_NEXTEST='$gate_nextest' (want -p/-test selectors or WORKSPACE)" >&2
            gate_nextest=""
            gate_nextest_expr=""
            gate_nextest_extra=""
            ;;
    esac
    case "$gate_nextest_extra" in
        '' | -p\ * | --test\ *) ;;
        *)
            # A malformed secondary partition cannot be dropped safely: its
            # tests are absent from the primary by construction. Fail safe to
            # the ordinary workspace selection instead.
            echo "check.sh: ignoring unusable WITCHY_GATE_NEXTEST_EXTRA='$gate_nextest_extra'" >&2
            gate_nextest=""
            gate_nextest_expr=""
            gate_nextest_extra=""
            ;;
    esac
    nextest_sel=()
    if [ -n "$gate_nextest" ]; then
        # shellcheck disable=SC2206 — selectors are `-p crate` / `--test name`.
        nextest_sel=($gate_nextest)
        # Queue fixtures live in the standalone
        # merge-queue-tests package, which root Cargo cannot resolve: a
        # selection mixing them with product targets (`--test merge_queue
        # --test rfc0092`, from a diff touching scripts/merge-queue.sh and a
        # root integration test) redded at t+0s with "no test target named
        # `merge_queue` in default-run package" (build/seed-from-gate,
        # 2026-09-23). Run them in the isolated queue shard, which selects
        # their package, and keep only the product targets here. A selection
        # of queue binaries alone keeps the main_queue_only path below.
        product_sel=()
        queue_sel_seen=0
        sel_index=0
        while [ "$sel_index" -lt "${#nextest_sel[@]}" ]; do
            if [ "${nextest_sel[$sel_index]}" = "--test" ]; then
                case "${nextest_sel[$((sel_index + 1))]:-}" in
                    merge_queue | test_for_paths | worktree)
                        queue_sel_seen=1
                        queue_target="${nextest_sel[$((sel_index + 1))]}"
                        queue_nextest_args="${queue_nextest_args:+$queue_nextest_args }--test $queue_target"
                        sel_index=$((sel_index + 2))
                        continue
                        ;;
                esac
            fi
            product_sel+=("${nextest_sel[$sel_index]}")
            sel_index=$((sel_index + 1))
        done
        if [ "$queue_sel_seen" -eq 1 ] && [ "${#product_sel[@]}" -gt 0 ]; then
            nextest_sel=("${product_sel[@]}")
            gate_nextest="${product_sel[*]}"
        fi
        # Mixed product/queue mappings cannot put a standalone queue target in
        # the product Cargo arguments, so test-for-paths records it only in the
        # expression (`binary(test_for_paths)`, etc.). Recover those identities
        # here and keep the isolated shard on the named binaries. Without this,
        # changing one router fixture beside a product test ran all 242 queue
        # tests even though the mapper had selected one queue binary.
        for queue_target in merge_queue test_for_paths worktree; do
            if [[ "$gate_nextest_expr" == *"binary($queue_target)"* ]] \
                && [[ " $queue_nextest_args " != *" --test $queue_target "* ]]; then
                queue_nextest_args="${queue_nextest_args:+$queue_nextest_args }--test $queue_target"
            fi
        done
        # Recognized mapped queue targets are sufficient to require their
        # shard, even when a helper path is absent from the coordinator's
        # infrastructure census. Otherwise a queue-only selection suppresses
        # the product run below without running any tests at all.
        if [ -n "$queue_nextest_args" ]; then
            queue_infra=1
            gate_scope=all
        fi
        if [ -n "$gate_nextest_expr" ]; then
            excl="($gate_nextest_expr)${excl:+ and ($excl)}"
        fi
        printf 'check.sh: serialized nextest selection: %s%s\n' \
            "$gate_nextest" "${gate_nextest_expr:+ -E $gate_nextest_expr}"
    else
        nextest_sel=(--workspace)
    fi
    nextest_extra_sel=()
    if [ -n "$gate_nextest_extra" ]; then
        # shellcheck disable=SC2206 -- mapper emits validated Cargo selectors.
        nextest_extra_sel=($gate_nextest_extra)
    fi
    nextest_cmd=(cargo nextest run "$max_fail")
    if [ -n "$gate_test_jobs" ]; then
        nextest_cmd+=(-j "$gate_test_jobs")
    fi
    nextest_cmd+=("${nextest_sel[@]}")
    # RFC-0165 A1: witchy-wir's native canonical/adapter tests must be part of
    # the ordinary workspace gate.  The package feature is not enabled merely
    # because the root package's default feature enables it as a dependency;
    # without this explicit selector, #[cfg(feature = "native")] tests in the
    # witchy-wir test target disappear while nextest can still report green.
    # Keep focused package selections valid: lowerer tests deliberately stay
    # on their pure inspection path; workspace gates opt into the native
    # lowerer campaign, while WIR-only batches still require WIR native tests.
    case " ${nextest_sel[*]} " in
        *" --workspace "*)
            nextest_cmd+=(--features witchy-wir/native,witchy-lower/native-test-runtime)
            ;;
        *" -p witchy-wir "*)
            # A mapper-selected --no-default-features WIR partition is the
            # audited pure SafeWIR route. Its explicit targets deliberately
            # omit the native-gate binary; full workspace and ordinary WIR
            # selections retain the mandatory native campaign.
            [[ " ${nextest_sel[*]} " == *" --no-default-features "* ]] \
                || nextest_cmd+=(--features witchy-wir/native)
            ;;
    esac
    nextest_extra_cmd=()
    if [ "${#nextest_extra_sel[@]}" -gt 0 ]; then
        nextest_extra_cmd=(cargo nextest run "$max_fail")
        [ -z "$gate_test_jobs" ] || nextest_extra_cmd+=(-j "$gate_test_jobs")
        nextest_extra_cmd+=("${nextest_extra_sel[@]}" --profile gate-partition)
        case " ${nextest_extra_sel[*]} " in
            *" -p witchy-wir "*)
                [[ " ${nextest_extra_sel[*]} " == *" --no-default-features "* ]] \
                    || nextest_extra_cmd+=(--features witchy-wir/native) ;;
        esac
    fi
    # The workspace stage ALWAYS excludes `package(merge-queue-tests)` (see
    # $excl above) because those fixtures run in their own isolated shard. So
    # a diff whose entire selection belongs to that package leaves this stage
    # with nothing, and nextest's
    # default is to FAIL on an empty selection. Observed 2026-08-27 on
    # fix/prewarm-preemption-flake: "Starting 0 tests across 0 binaries
    # (1 binary skipped)" -> "error: no tests to run" -> branch RED, even
    # though its tests ran green in the queue-infra shard moments later.
    #
    # Narrow deliberately: every selector must name one of the standalone
    # queue binaries, so a mapper bug that genuinely selects nothing or mixes
    # in a product target still fails loudly here.
    main_queue_only=0
    if [ "${#nextest_sel[@]}" -ge 2 ] \
        && [ "$(( ${#nextest_sel[@]} % 2 ))" -eq 0 ]; then
        main_queue_only=1
        selector_index=0
        while [ "$selector_index" -lt "${#nextest_sel[@]}" ]; do
            if [ "${nextest_sel[$selector_index]}" != "--test" ]; then
                main_queue_only=0
                break
            fi
            case "${nextest_sel[$((selector_index + 1))]}" in
                merge_queue | test_for_paths | worktree) ;;
                *) main_queue_only=0; break ;;
            esac
            selector_index=$((selector_index + 2))
        done
    fi
    # RFC-0159 follow-up: replay the tests that red the PREVIOUS attempt for this
    # change, before the full suite.
    #
    # The coordinator re-runs the whole suite on every retry, so a branch that
    # keeps failing the same test pays full test-execution cost to rediscover
    # it -- RFC-0158 did that eight times. Retries do not get cheaper either:
    # measured over 173 paired attempts, TRY2/TRY1 duration is p50 1.00.
    #
    # Nine recent green-preflight gates immediately repeated ten passing tests
    # in the main run: 169.137s of summed per-test work. The tests overlap, so
    # the defensible wall-time saving is smaller (roughly 1-55s per affected
    # gate, commonly 12-30s when a serialized performance test is replayed).
    #
    # Preflight and the main run share one fully composed command context. For
    # an exact workspace selection, a green preflight satisfies its
    # intersection with the landing selection and the main invocation runs the
    # exact remainder. Their union is the original selection. Remembered
    # failures are capped at eight by the coordinator, while the workspace has
    # thousands of selected tests, so subtraction cannot create an empty main
    # run. Scoped selections keep the replay in the main run: without a
    # machine-readable preflight match count, `--no-tests=warn` on a possibly
    # exhausted remainder could hide a mapper error.
    preflight_cmd=()
    if [ -n "${WITCHY_GATE_PREFLIGHT_EXPR:-}" ]; then
        # --no-tests=warn: the replay expression names tests that red the LAST
        # attempt, but this attempt's selection is derived fresh from the diff,
        # so those tests may not be in scope at all (the branch moved, the test
        # was renamed, or the mapper picked different packages). nextest's
        # default is to FAIL on an empty selection, which marked the branch red
        # for a reason that has nothing to do with the branch: observed
        # 2026-08-27 on impl/rfc0161-typed-queue, where the replay matched 0 of
        # 2032 tests and red the gate after a 3m28s build. Preflight is an
        # optimization -- when it has nothing to replay the correct outcome is
        # to fall through to the full run. `warn` over `pass` so the empty
        # replay still shows up in the log.
        preflight_expr="($excl) and (${WITCHY_GATE_PREFLIGHT_EXPR})"
        preflight_base_cmd=("${nextest_cmd[@]}")
        preflight_target=()
        if [ -n "${WITCHY_GATE_PREFLIGHT_NEXTEST:-}" ]; then
            # The coordinator emits one bounded form only when every failed
            # test names the same nextest package/target. Validate it again
            # before word splitting so corrupted local state cannot add Cargo
            # arguments. `read -a` splits without pathname expansion.
            read -r -a preflight_target <<<"${WITCHY_GATE_PREFLIGHT_NEXTEST}"
            preflight_target_valid=0
            if [ "${#preflight_target[@]}" -eq 3 ] \
                && [ "${preflight_target[0]}" = "-p" ] \
                && [[ "${preflight_target[1]}" =~ ^[A-Za-z0-9_-]+$ ]] \
                && [ "${preflight_target[2]}" = "--lib" ]; then
                preflight_target_valid=1
            elif [ "${#preflight_target[@]}" -eq 4 ] \
                && [ "${preflight_target[0]}" = "-p" ] \
                && [[ "${preflight_target[1]}" =~ ^[A-Za-z0-9_-]+$ ]] \
                && { [ "${preflight_target[2]}" = "--test" ] \
                    || [ "${preflight_target[2]}" = "--bin" ]; } \
                && [[ "${preflight_target[3]}" =~ ^[A-Za-z0-9_-]+$ ]]; then
                preflight_target_valid=1
            fi
            # A scoped landing selection can still name the failed package.
            # Narrow its replay target in that case too; otherwise a one-test
            # retry rebuilds every selected package before running the probe.
            # Keep an unselected target broad so stale queue state cannot add a
            # package outside the branch's current admission surface.
            preflight_package="${preflight_target[1]}"
            if [ "$preflight_target_valid" -eq 1 ]; then
                if ! { { [ "${#nextest_sel[@]}" -eq 1 ] \
                    && [ "${nextest_sel[0]}" = "--workspace" ]; } \
                    || printf ' %s ' "$gate_nextest $gate_nextest_extra" \
                        | grep -Fq " -p $preflight_package "; }; then
                    preflight_target_valid=0
                fi
            fi
            if [ "$preflight_target_valid" -eq 1 ]; then
                preflight_kind="${preflight_target[2]#--}"
                preflight_name=""
                [ "$preflight_kind" = "lib" ] \
                    || preflight_name="${preflight_target[3]}"
                preflight_metadata="$(cargo metadata --no-deps --format-version 1 2>/dev/null || true)"
                if ! printf '%s\n' "$preflight_metadata" | jq -e \
                    --arg package "$preflight_package" --arg kind "$preflight_kind" \
                    --arg name "$preflight_name" '
                        any(.packages[]?;
                            .name == $package and
                            any(.targets[]?;
                                (.kind | index($kind)) != null and
                                ($kind == "lib" or .name == $name)))
                    ' >/dev/null 2>&1; then
                    preflight_target_valid=0
                fi
            fi
            if [ "$preflight_target_valid" -eq 1 ]; then
                preflight_base_cmd=(cargo nextest run "$max_fail")
                [ -z "$gate_test_jobs" ] || preflight_base_cmd+=(-j "$gate_test_jobs")
                preflight_base_cmd+=("${preflight_target[@]}")
                case "$preflight_package" in
                    witchy | witchy-wir)
                        if [ "$preflight_package" != witchy-wir ] \
                            || [[ " ${nextest_sel[*]} " != *" --no-default-features "* ]]; then
                            preflight_base_cmd+=(--features witchy-wir/native)
                        fi
                        ;;
                    witchy-lower)
                        if [ "${#nextest_sel[@]}" -eq 1 ] \
                            && [ "${nextest_sel[0]}" = "--workspace" ]; then
                            preflight_base_cmd+=(--features witchy-wir/native,witchy-lower/native-test-runtime)
                        fi
                        ;;
                esac
                printf 'check.sh: failure preflight target: %s\n' \
                    "${WITCHY_GATE_PREFLIGHT_NEXTEST}"
            fi
        fi
        preflight_cmd=("${preflight_base_cmd[@]}" --no-tests=warn -E "$preflight_expr")
        if [ "${#nextest_sel[@]}" -eq 1 ] \
            && [ "${nextest_sel[0]}" = "--workspace" ]; then
            main_expr="($excl) and not (${WITCHY_GATE_PREFLIGHT_EXPR})"
        else
            main_expr="$excl"
        fi
    else
        main_expr="$excl"
    fi
    if [ "$main_queue_only" -eq 1 ]; then
        # These targets belong to the standalone merge-queue-tests package.
        # Asking root Cargo to resolve them fails before the exclusion can
        # produce an empty selection. The isolated shard below runs them with
        # the owning package selected.
        preflight_cmd=()
        test_cmd=(true)
        test_cmd_extra=()
    else
        test_cmd=("${nextest_cmd[@]}")
        # Explicit `--test <binary>` targets are compiled before filtering (a
        # misspelled one fails at target lookup), so a selection whose every
        # admitted test is deferred is a compile check, not a failure. Any
        # broader selector keeps nextest's loud empty-selection error.
        explicit_targets_only=0
        if [ "${#nextest_sel[@]}" -ge 2 ] \
            && [ "$(( ${#nextest_sel[@]} % 2 ))" -eq 0 ]; then
            explicit_targets_only=1
            selector_index=0
            while [ "$selector_index" -lt "${#nextest_sel[@]}" ]; do
                if [ "${nextest_sel[$selector_index]}" != "--test" ]; then
                    explicit_targets_only=0
                    break
                fi
                selector_index=$((selector_index + 2))
            done
        fi
        [ "$explicit_targets_only" -eq 0 ] || test_cmd+=(--no-tests=warn)
        [ -z "$main_expr" ] || test_cmd+=(-E "$main_expr")
        test_cmd_extra=()
        if [ "${#nextest_extra_cmd[@]}" -gt 0 ]; then
            # macOS Bash 3.2 treats an empty-array expansion as unbound under
            # `set -u`, even after the array has been initialized above.
            test_cmd_extra=("${nextest_extra_cmd[@]}")
            [ -z "$main_expr" ] || test_cmd_extra+=(-E "$main_expr")
        fi
    fi

    # The merge coordinator can hand the committed base directly to dtest.
    # Dtest then owns selection and nextest execution in one process while the
    # mapper retains the audited Cargo target scope. The report is post-run
    # telemetry and is not a control protocol.
    # Keep the legacy command above intact as the fail-safe when selection
    # fails or the heuristic reaches no tests.
    dtest_cmd=()
    if [ -n "${WITCHY_GATE_DTEST_FROM:-}" ] \
        && [ -n "${WITCHY_GATE_DTEST_REPORT:-}" ] \
        && [ "$main_queue_only" -eq 0 ] \
        && [ "${#test_cmd_extra[@]}" -eq 0 ] \
        && command -v dtest >/dev/null 2>&1; then
        dtest_cmd=(dtest --workspace . run
            --from "$WITCHY_GATE_DTEST_FROM" --to HEAD --assume-clean
            --caller-scope --narrow-single-partition --ignore-config-runner-args
            --quiet --report "$WITCHY_GATE_DTEST_REPORT")
        [ -z "$main_expr" ] || dtest_cmd+=(--filter-expr "$main_expr")
        # Reuse the mapper's validated Cargo target and feature scope, dropping
        # only the `cargo nextest run` executable prefix. Dtest contributes the
        # exact-test expression and launches this as one nextest process.
        dtest_cmd+=(-- "${nextest_cmd[@]:3}")
        [ "$explicit_targets_only" -eq 0 ] || dtest_cmd+=(--no-tests=warn)
    fi

    # Gate only: the pure SafeWIR and lowerer-only routes resolve a different
    # feature graph for every compiler crate from cap-model up. The former says
    # `--no-default-features` explicitly; the Cargo shim isolates the latter
    # when witchy-lower is selected without a native owner. Sharing either one
    # with the native graph made consecutive gates rebuild the whole compiler
    # stack each time the route flipped. Each graph keeps its own warm target.
    pure_gate_command() {
        local command=" $* "
        [[ "$command" == *" --no-default-features "* ]] && return 0
        [[ "$command" == *" -p witchy-lower "* \
            || "$command" == *" --package witchy-lower "* \
            || "$command" == *" --package=witchy-lower "* ]] || return 1
        [[ "$command" != *" --workspace "* \
            && "$command" != *" -p witchy "* \
            && "$command" != *" --package witchy "* \
            && "$command" != *" --package=witchy "* \
            && "$command" != *" witchy-wir/native"* \
            && "$command" != *" witchy-lower/native-test-runtime"* ]]
    }
    if [ -n "${WITCHY_GATE_SCOPE:-}" ]; then
        if pure_gate_command "${test_cmd[@]}"; then
            test_cmd=(env "CARGO_TARGET_DIR=${target_dir}-pure" "${test_cmd[@]}")
        fi
        if [ "${#test_cmd_extra[@]}" -gt 0 ] \
            && pure_gate_command "${test_cmd_extra[@]}"; then
            test_cmd_extra=(env "CARGO_TARGET_DIR=${target_dir}-pure" "${test_cmd_extra[@]}")
        fi
    fi

    # Every queue fixture owns a distinct temporary repository, state root, and
    # process group. Bounded width removes the serialized fixture floor while
    # retaining an operator override for repeated reliability measurements.
    # Alternating on 2026-09-24 under the same loaded host, width 6 ran all 228
    # tests in 381s versus 453s at width 3 (-16% wall, +7% CPU+sys).
    # The coordinator's mapper names queue integration binaries alongside
    # product targets, then this script removes them from root Cargo above.
    # Preserve that exact queue selection here instead of charging every
    # worktree/router edit for all coordinator fixtures. An absent or unusable
    # selection remains fail-safe and runs the complete three-binary shard.
    queue_infra_targets=(--test merge_queue --test test_for_paths --test worktree)
    if [ -n "$queue_nextest_args" ]; then
        # shellcheck disable=SC2206 -- values are validated queue target names.
        queue_infra_targets=($queue_nextest_args)
    fi
    queue_infra_cmd=(cargo nextest run -p merge-queue-tests "${queue_infra_targets[@]}" -j "$queue_infra_jobs" \
        --ignore-default-filter --profile nightly)
    # A queue-only serialized selection can carry a fail-safe module filter
    # from test-for-paths.sh. Product expressions are never forwarded here:
    # mixed diffs clear the narrow filter before the mapper emits it.
    if [ "$main_queue_only" -eq 1 ] && [ -n "$gate_nextest_expr" ]; then
        queue_infra_cmd+=(-E "$gate_nextest_expr")
    fi
    repo_contracts_cmd=(cargo nextest run -p repo-tests --test repo_hygiene)
    # Keep the long coordinator-reexec case and the sub-second timing-accounting
    # case in the complete landing shard, not this fast edit sample. On
    # 2026-09-24 repeated exact-tip presubmits spent 4-6 minutes retrying them:
    # reexec missed a 70-90s wall deadline, while a fake one-second Clippy sleep
    # measured three seconds and violated its <=2s assertion under host load.
    # The immediately preceding serialized landing shards passed both. They
    # remain covered before merge without making every queue edit depend on
    # wall-clock outliers.
    queue_infra_edit_cmd=("${queue_infra_cmd[@]}" -E \
        'test(/^(coordinator::(a_code_submission_without_a_receipt_still_warns|a_red_remembers_its_failing_tests_for_the_next_attempt|mixed_failure_targets_keep_the_broad_preflight_build_scope|concurrent_daemon_starts_create_exactly_one_coordinator|concurrent_dependency_updates_cannot_commit_opposite_cycle_edges|queue_substrate_change_requests_the_isolated_fixture_shard|ready_queue_items_run_before_post_merge_worktree_sweep|submit_journals_whether_a_presubmit_receipt_covers_the_tip|submit_wrapper_changes_are_presubmit_eligible_queue_infrastructure|sweep_batches_historical_branch_pruning|sweep_caches_merged_history_but_keeps_queue_admission_live|sweep_defers_branch_pruning_while_ready_work_waits|sweep_skips_only_unchanged_superseded_refs_already_swept|sweep_yields_when_ready_work_arrives_during_branch_pruning)|gate_behavior::preflight_reuse::(workspace_preflight_narrows_its_target_and_partitions_the_main_run|invalid_or_missing_preflight_target_fails_safe_to_the_workspace|scoped_preflight_keeps_the_complete_main_selection)|gate_behavior::state_and_gate::(gate_report_is_read_only_and_aggregates_batches_failures_and_phases|state_path_prefers_fresh_canonical_layout_and_preserves_legacy_until_cutover|unrelated_red_batch_evicts_the_culprit_and_rebatches_the_rest)|packaged_source_surfaces_run_the_unsafe_input_probe_first|check_script_gate_runs_only_its_direct_behavior_modules)$/)')
else
    preflight_cmd=()
    # Plain cargo test runs integration binaries sequentially, so the queue
    # fixture is already isolated from other binaries in this fallback mode.
    # Keep the fallback identical to the nextest/CI path: package-local
    # `native` features are not inherited from the root package's defaults.
    # Without this explicit feature, witchy-wir's native security tests are
    # compiled out while cargo still reports a green workspace test run.
    test_cmd=(cargo test --workspace --features witchy-wir/native,witchy-lower/native-test-runtime)
    test_cmd_extra=()
    queue_infra_cmd=(cargo test -p merge-queue-tests --test merge_queue --test test_for_paths --test worktree -- --test-threads=1)
    repo_contracts_cmd=(cargo test -p repo-tests --test repo_hygiene)
    # Plain Cargo accepts only one substring filter, which cannot express the
    # representative cross-module selection without dropping coverage. Keep
    # the complete shard in this uncommon fallback.
    queue_infra_edit_cmd=("${queue_infra_cmd[@]}")
fi

run_product_tests() {
    if [ "${#test_cmd_extra[@]}" -gt 0 ]; then
        "${test_cmd_extra[@]}" || return "$?"
    fi
    if [ "${#dtest_cmd[@]}" -gt 0 ]; then
        rm -f "$WITCHY_GATE_DTEST_REPORT"
        local dtest_status=0 selected_tests=""
        "${dtest_cmd[@]}" || dtest_status=$?
        if [ -s "$WITCHY_GATE_DTEST_REPORT" ]; then
            selected_tests="$(jq -er '
                select(.schema == 1 and (.tests|type) == "array")
                | .tests | length
            ' "$WITCHY_GATE_DTEST_REPORT" 2>/dev/null || true)"
            if [ -n "$selected_tests" ] && [ "$selected_tests" -gt 0 ]; then
                if [ "$dtest_status" -ne 4 ]; then
                    return "$dtest_status"
                fi
                # Nextest exit 4 means this dtest chunk selected no runnable
                # test inside the mapper's Cargo target. Dtest's report counts
                # tests before that target intersection, so a positive report
                # alone cannot prove a test ran. Keep the audited mapper's
                # scoped command as the fallback; do not treat an empty run as
                # a green gate.
                echo "check.sh: dtest target intersection was empty; using audited legacy selection" >&2
            else
                if [ -z "$selected_tests" ] && [ "$dtest_status" -ne 0 ]; then
                    echo "check.sh: dtest produced an invalid report after runner failure" >&2
                    return "$dtest_status"
                fi
                echo "check.sh: dtest selected no tests; using audited legacy selection" >&2
            fi
        else
            echo "check.sh: dtest selection failed; using audited legacy selection" >&2
        fi
    fi
    if [ -z "$queue_overlap_dir" ]; then
        "${test_cmd[@]}"
        return
    fi
    # Copy the product stream so the overlap launcher can see nextest's run
    # announcement. pipefail keeps nextest's status, not tee's.
    local product_status=0
    "${test_cmd[@]}" 2>&1 | tee -a "$queue_overlap_dir/product.log" || product_status=$?
    : >"$queue_overlap_dir/product.done"
    return "$product_status"
}

# Queue-fixture overlap (see WITCHY_GATE_QUEUE_INFRA_OVERLAP above). The
# launcher waits for the product run's `Nextest run ID` line, the stable
# boundary after which its Cargo build has released the target, then runs the
# fixture shard with output captured to a file. `queue_infra_stage` later
# replays that file in the ordinary "queue infrastructure" stage and returns
# the shard's status, so the watchdog, red classification, and failing-test
# extraction see the same log shape as the sequential shard. If the product
# run never announced itself, nothing started and the stage runs the shard in
# the foreground exactly as before.
queue_overlap_dir=""
queue_overlap_launcher=""
arm_queue_overlap() {
    [ "$queue_infra" -eq 1 ] && [ "$queue_infra_overlap" -eq 1 ] || return 0
    [ "${test_cmd[*]}" != true ] || return 0
    cargo nextest --version >/dev/null 2>&1 || return 0
    queue_overlap_dir="$(mktemp -d "${TMPDIR:-/tmp}/witchy-queue-overlap-XXXXXX")"
    : >"$queue_overlap_dir/product.log"
    local dir="$queue_overlap_dir"
    (
        while ! grep -aqF 'Nextest run ID ' "$dir/product.log" 2>/dev/null; do
            if [ -e "$dir/product.done" ]; then : >"$dir/not-started"; exit 0; fi
            sleep 0.5
        done
        date +%s >"$dir/started"
        # The product build just produced these binaries; an agent's seed
        # refresh must not swap files under the product run still executing.
        WITCHY_SEED_REFRESH=off "${queue_infra_cmd[@]}" >"$dir/queue.log" 2>&1 &
        echo "$!" >"$dir/queue.pid"
        shard_status=0
        wait "$!" || shard_status=$?
        date +%s >"$dir/finished"
        echo "$shard_status" >"$dir/status"
    ) &
    queue_overlap_launcher=$!
    trap stop_queue_overlap EXIT
}

stop_queue_overlap() {
    [ -n "$queue_overlap_dir" ] || return 0
    local dir="$queue_overlap_dir"
    queue_overlap_dir=""
    : >"$dir/product.done" 2>/dev/null || true
    if [ -s "$dir/queue.pid" ] && [ ! -e "$dir/status" ]; then
        kill -TERM "$(cat "$dir/queue.pid")" 2>/dev/null || true
    fi
    wait "$queue_overlap_launcher" 2>/dev/null || true
    rm -rf "$dir"
}

queue_infra_stage() {
    local dir="$queue_overlap_dir"
    if [ -z "$dir" ]; then
        "${queue_infra_cmd[@]}"
        return
    fi
    : >"$dir/product.done"
    while [ ! -e "$dir/started" ] && [ ! -e "$dir/not-started" ]; do
        sleep 0.1
    done
    if [ ! -e "$dir/started" ]; then
        stop_queue_overlap
        "${queue_infra_cmd[@]}"
        return
    fi
    local started; started="$(cat "$dir/started")"
    printf 'check.sh: queue fixtures started at t+%ds beside the product test run\n' \
        "$(( started - t_start ))"
    local offset=0 size shard_done
    while :; do
        shard_done=0
        [ -e "$dir/status" ] && shard_done=1
        size="$(wc -c <"$dir/queue.log" | tr -d ' ')"
        if [ "$size" -gt "$offset" ]; then
            tail -c "+$((offset + 1))" "$dir/queue.log" | head -c "$((size - offset))"
            offset="$size"
        fi
        [ "$shard_done" -eq 1 ] && break
        sleep 1
    done
    local shard_status finished
    shard_status="$(cat "$dir/status")"
    finished="$(cat "$dir/finished")"
    local shard_state="green"
    [ "$shard_status" -eq 0 ] || shard_state="red"
    emit_timing background "$step" "queue infrastructure (overlapped shard)" \
        "$shard_state" "$started" "$finished"
    stop_queue_overlap
    return "$shard_status"
}

# Progress channel for the bounded macOS list phase: the list wrapper appends
# one line per test binary it starts (scripts/nextest-list-wrapper.sh), and
# stage_heartbeat converts growth into REAL log lines that reset its bounded
# pulse window. Needed because first-exec of a freshly linked ~100MB binary is
# pure kernel-side codesign verification: ~25s wall with 0.00 user AND 0.00
# sys CPU and no output — invisible to both the coordinator's idle check and
# log liveness. An interp-touching diff relinks all ~47 binaries, so its list
# phase is minutes of legitimate, observable-only-here work (this killed four
# gates on 2026-07-16 before the channel existed).
export WITCHY_LIST_PROGRESS_FILE="$(mktemp "${TMPDIR:-/tmp}/witchy-list-progress-XXXXXX")"

# A named shard runs exactly one section and exits (reporting elapsed time).
# The witchy formatter over std, examples, and projects (NOT rustfmt over the
# Rust — that's hand-formatted and out of scope). Uses the binary built above.
# ONE invocation over every file, not one process per file: `witchy fmt --check`
# already processes all path args, names each unformatted file on stderr, and
# exits 1 iff any fails — so the per-file loop only paid ~200 extra process
# spawns (1.5s vs 0.13s over the 205 files) for identical diagnostics.
# House prose style bans the em dash in documentation prose (spaced hyphen
# instead). Fenced code blocks and inline code spans are exempt: an em dash
# there is example content or program output, not prose. spec/stdlib.md is
# generated from std/*.witchy doc comments, so it is checked at the source.
prose_em_dash_check() {
    python3 - <<'PY'
import glob, re, sys
targets = ["README.md", "SECURITY.md", "CONTRIBUTING.md", "KNOWN-ISSUES.md"]
targets += sorted(glob.glob("book/src/*.md"))
targets += [f for f in sorted(glob.glob("spec/*.md")) if f != "spec/stdlib.md"]
targets += sorted(glob.glob("std/*.witchy"))
bad = 0
for path in targets:
    text = open(path, encoding="utf-8").read()
    if path.endswith(".witchy"):
        segments = [l for l in text.split("\n") if l.lstrip().startswith("//")]
    else:
        # even-indexed splits are prose; odd are fenced blocks / inline code
        segments = re.split(r"(```.*?```|`[^`\n]*`)", text, flags=re.S)[::2]
    for seg in segments:
        for _ in re.finditer("—", seg):
            bad += 1
            print(f"{path}: em dash in prose (use a spaced hyphen ' - ')",
                  file=sys.stderr)
            break
sys.exit(1 if bad else 0)
PY
}

# Record that THIS commit passed a focused shard, so `merge-queue.sh submit`
# can tell a qualified branch from an unvalidated one.
#
# Why this exists: over 7 days, 51% of all merge-gate seconds went to attempts
# that never landed, and 168 of 179 genuine reds (10.8h -- 21% of ALL gate
# time) failed in the `tests` stage. That is precisely what `--fast` runs, so
# those were failures a pre-submit shard would have caught. Each one burns a
# whole serialized gate while every queued branch waits.
#
# Keyed by COMMIT, not branch: amending or adding a commit must invalidate the
# receipt, because the thing that was validated no longer exists. Receipts are
# advisory metadata -- a failure to write one must never fail a green shard,
# so every step here tolerates error.
#
# Serialized coordinator gates are skipped (WITCHY_GATE_SCOPE is set): that IS
# the gate, not evidence gathered before submitting to it.
record_shard_receipt() { # record_shard_receipt <shard-label>
    ./scripts/record-shard-receipt.sh "$1" || true
    return 0
}

witchy_fmt_check() {
    local files=()
    local f
    for f in std/*.witchy examples/*/src/*.witchy; do
        [ -f "$f" ] && files+=("$f")
    done
    while IFS= read -r f; do
        files+=("$f")
    done < <(find projects -type f -path '*/src/*.witchy' -print 2>/dev/null | sort)
    [ "${#files[@]}" -eq 0 ] && return 0
    # Crate-only nextest does not build the CLI. A syntax/src/std batch that
    # still runs fmt may need to link it here; types/lower/interp skip fmt.
    if [ ! -x "$target_dir/debug/witchy" ]; then
        cargo build --bin witchy
    fi
    "$target_dir/debug/witchy" fmt --check "${files[@]}"
}

# RFC-0111's ordinary gate leg is deliberately untimed: it proves that every
# paired Witchy/Rust case agrees with its independent result. On ARM it also
# proves that the Rust translation units contain no packed-vector instructions;
# other supported gate hosts report correctness-only rather than claiming
# scalar evidence. Build the Witchy executable with the exact candidate commit
# embedded, then make the harness authenticate that identity before it accepts
# the results.
rust_class_check() {
    local head witchy_bin rust_build_dir witchy_version=""
    head=$(git rev-parse --verify HEAD)
    case "$target_dir" in
        /*)
            witchy_bin="$target_dir/debug/witchy"
            rust_build_dir="$target_dir/rust-class"
            ;;
        *)
            witchy_bin="$PWD/$target_dir/debug/witchy"
            rust_build_dir="$PWD/$target_dir/rust-class"
            ;;
    esac
    if [ -x "$witchy_bin" ]; then
        witchy_version=$("$witchy_bin" --version 2>/dev/null | head -1 || true)
    fi
    case "$witchy_version" in
        *"(commit $head)"*) ;;
        *) WITCHY_BUILD_COMMIT="$head" cargo build -p witchy --bin witchy --quiet ;;
    esac
    WITCHY="$witchy_bin" WITCHY_RUST_BUILD_DIR="$rust_build_dir" \
        ./bench/rust-class/run.sh --check
    # (RFC-0111 criterion 9) Timing-gate activation: the reviewed report from the
    # pinned ARM reference machine is committed at bench/rust-class/reports/. The
    # gate re-verifies its versioned shape, commit/binary provenance, independent
    # result oracle, scalar-verifier certification, and the 1.25x geomean / 1.50x
    # per-case thresholds on every run. `--verify-report` is self-contained
    # (validates the report's embedded commit, not HEAD), so it stays valid across
    # later commits and needs no rebuild. A tampered or regressed report fails here.
    local pinned_report="bench/rust-class/reports/arm64-reference.json"
    if [ -f "$pinned_report" ]; then
        ./bench/rust-class/run.sh --verify-report "$pinned_report"
    fi
}

# nextest builds what it needs, so no separate build step; without nextest the
# filters don't exist, so the shards require it.
if [ -n "$shard" ]; then
    shard_t0=$(date +%s)
    case "$shard" in
        examples)
            cargo nextest --version >/dev/null 2>&1 || { echo "check.sh: --examples requires cargo-nextest" >&2; exit 2; }
            cargo nextest run --workspace --features witchy-wir/native -E "test(/^example_tests::/)${deferred_excl:+ and $deferred_excl}"
            ;;
        queue-infra)
            cargo nextest --version >/dev/null 2>&1 || { echo "check.sh: --queue-infra requires cargo-nextest" >&2; exit 2; }
            "${queue_infra_cmd[@]}"
            ;;
        queue-infra-edit)
            cargo nextest --version >/dev/null 2>&1 || { echo "check.sh: --queue-infra-edit requires cargo-nextest" >&2; exit 2; }
            "${queue_infra_edit_cmd[@]}"
            ;;
    esac
    record_shard_receipt "$shard"
    printf '\n\033[1;32mshard %s green\033[0m in %ds\n' "$shard" "$(( $(date +%s) - shard_t0 ))"
    exit 0
fi

# Each stage marker carries its offset from gate start (`==> [1] tests (workspace) (t+0s)`),
# so a redirected log is enough to see what is running now (merge-queue.sh
# status/doctor parse these markers), and each stage reports its own duration
# on completion so slow regressions are visible per stage, not just in total.
step=0
t_start=$(date +%s)

# Machine-readable timing rides in the ordinary gate log so observability adds
# no lock, sidecar writer, or coordinator failure mode. Labels are internal
# constants (never user input), which keeps this Bash-3.2-compatible emitter
# dependency-free. Older reports ignore the prefix; gate-report.sh prefers it
# when present and falls back to the human markers above for historical logs.
emit_timing() { # emit_timing <kind> <step> <label> <status> <started> <finished>
    local kind="$1" timing_step="$2" label="$3" status="$4" started="$5" finished="$6"
    printf 'WITCHY_TIMING {"schema":1,"kind":"%s","step":%d,"name":"%s","status":"%s","started_epoch":%d,"finished_epoch":%d,"elapsed_s":%d,"gate_elapsed_s":%d}\n' \
        "$kind" "$timing_step" "$label" "$status" "$started" "$finished" \
        "$(( finished - started ))" "$(( finished - t_start ))"
}

# A stage can legitimately go quiet after Cargo finishes compiling and before
# nextest completes its first test binary. Emit only a bounded number of pulses:
# enough to bridge that startup window, but not enough to hide a true deadlock
# from the coordinator's idle-log watchdog indefinitely. Standalone runs default
# to three; the serialized gate exports eight for bounded macOS list discovery.
stage_heartbeat() { # stage_heartbeat <step> <label> <stage-start>
    local stage_step="$1" label="$2" stage_start="$3"
    local interval="${WITCHY_STAGE_HEARTBEAT_INTERVAL:-120}"
    local limit="${WITCHY_STAGE_HEARTBEAT_LIMIT:-3}"
    local pulse=0 sleep_pid=""
    case "$interval:$limit" in
        *[!0-9:]* | :* | *:) return 0 ;;
    esac
    [ "$interval" -gt 0 ] && [ "$limit" -gt 0 ] || return 0
    trap 'kill "${sleep_pid:-}" 2>/dev/null || true; exit 0' TERM INT
    local progress_seen=0 progress_now=0
    while [ "$pulse" -lt "$limit" ]; do
        sleep "$interval" &
        sleep_pid=$!
        wait "$sleep_pid" || return 0
        sleep_pid=""
        # Observable progress RESETS the bounded window: these lines are real
        # signal (the coordinator's liveness counts them, unlike the bounded
        # pulses below), so a stage that keeps verifying+listing binaries may
        # pulse indefinitely, while a truly stalled stage still exhausts the
        # limit and goes silent for the watchdog exactly as before.
        if [ -n "${WITCHY_LIST_PROGRESS_FILE:-}" ] && [ -f "$WITCHY_LIST_PROGRESS_FILE" ]; then
            progress_now="$(wc -l <"$WITCHY_LIST_PROGRESS_FILE" 2>/dev/null | tr -d ' ')"
            case "$progress_now" in '' | *[!0-9]*) progress_now=0 ;; esac
            if [ "$progress_now" -gt "$progress_seen" ]; then
                printf '\033[1;34m    [%d] %s list progress: %d test binaries started (stage+%ds)\033[0m\n' \
                    "$stage_step" "$label" "$progress_now" "$(( $(date +%s) - stage_start ))"
                progress_seen="$progress_now"
                pulse=0
                continue
            fi
        fi
        pulse=$((pulse + 1))
        printf '\033[1;34m    [%d] %s still running (heartbeat %d/%d, stage+%ds)\033[0m\n' \
            "$stage_step" "$label" "$pulse" "$limit" "$(( $(date +%s) - stage_start ))"
    done
    # Keep this process alive after the bounded pulses so its pid cannot be
    # recycled before `run` kills and reaps it when the stage eventually ends.
    sleep 2147483647 &
    sleep_pid=$!
    wait "$sleep_pid" || return 0
}

run() {
    step=$((step + 1))
    local t_stage; t_stage=$(date +%s)
    printf '\n\033[1;34m==> [%d] %s (t+%ds)\033[0m\n' "$step" "$1" "$(( t_stage - t_start ))"
    local label="$1"
    shift
    stage_heartbeat "$step" "$label" "$t_stage" &
    stage_heartbeat_pid=$!
    local command_status=0
    "$@" || command_status=$?
    kill "$stage_heartbeat_pid" 2>/dev/null || true
    wait "$stage_heartbeat_pid" 2>/dev/null || true
    stage_heartbeat_pid=""
    local t_finished; t_finished=$(date +%s)
    local stage_status="green"
    [ "$command_status" -eq 0 ] || stage_status="red"
    emit_timing foreground "$step" "$label" "$stage_status" "$t_stage" "$t_finished"
    [ "$command_status" -eq 0 ] || return "$command_status"
    printf '\033[1;34m    [%d] %s took %ds\033[0m\n' "$step" "$label" "$(( t_finished - t_stage ))"
}

if [ "$fast" -eq 1 ]; then
    # The fast commit gate: nextest compiles+links its own test binaries, so no
    # standalone build or lint step is needed.
    # Run the witchy fmt check IF any .witchy files are dirty — catches formatting
    # mistakes without the cost of always building the binary + sweeping 200 files.
    # (This build is FOREGROUND in the MAIN target dir; nextest below reuses
    # its artifacts.)
    if git diff --name-only HEAD 2>/dev/null | grep -q '\.witchy$'; then
        run "build (binary)"           cargo build -p witchy
        run "witchy fmt (changed .witchy files)" witchy_fmt_check
    fi
    if git diff --name-only HEAD 2>/dev/null | grep -qE '\.(md|witchy)$'; then
        run "prose style (no em dashes)" prose_em_dash_check
    fi
    if [ "${#preflight_cmd[@]}" -gt 0 ]; then
        run "preflight (tests that red the last attempt)" "${preflight_cmd[@]}"
    fi
    # Product discovery already builds the excluded merge_queue test binary.
    # Its hermetic shard reuses that artifact, starting once the product build
    # is done and overlapping the product test run.
    arm_queue_overlap
    run "tests (workspace)" run_product_tests
    [ "$queue_infra" -eq 0 ] || run "queue infrastructure (isolated)" queue_infra_stage
    record_shard_receipt fast
    printf '\n\033[1;32mfast gate green\033[0m — run without --fast before push (fmt + landing checks)\n'
    exit 0
fi

if [ "$gate_scope" = "docs" ]; then
    case " ${WITCHY_GATE_NEXTEST:-} " in
        *" -p repo-tests "*)
            run "repository contracts (docs-only)" "${repo_contracts_cmd[@]}" ;;
        *)
            run "docs-only scope — heavy stages skipped" true ;;
    esac
    printf '\n\033[1;32mall green\033[0m — WITCHY_GATE_SCOPE=docs: compiler tests/check/fmt skipped (post-merge CI runs the full suite)\n'
    exit 0
fi

if [ "$gate_scope" = "cost-tool" ]; then
    run "gate cost tooling" python3 scripts/test-gate-test-cost.py
    run "prose style (no em dashes)" prose_em_dash_check
    printf '\n\033[1;32mall green\033[0m — WITCHY_GATE_SCOPE=cost-tool\n'
    exit 0
fi

# The compiler's literal std calls must be rooted before any product test can
# trust pruning. A stale generated list otherwise fails one type-library test
# late in a multi-thousand-test workspace run. This exact generator check needs
# no Cargo build and fails the serialized gate before it starts compiler jobs.
if [ -n "${WITCHY_GATE_SCOPE+x}" ]; then
    if ! python3 scripts/gen-std-pruning-roots.py --check; then
        echo "check.sh: compiler std roots are stale; run scripts/gen-std-pruning-roots.py" >&2
        exit 1
    fi
fi

# Nextest builds the selected production and test targets, including the
# `witchy` binary named by CARGO_BIN_EXE_witchy. A separate cargo-check graph
# duplicates compilation and competes with the gate's critical path.

if [ "${#preflight_cmd[@]}" -gt 0 ]; then
    run "preflight (tests that red the last attempt)" "${preflight_cmd[@]}"
fi
arm_queue_overlap
run "tests (workspace)"        run_product_tests
# Test execution is a few seconds; compiling its binaries is the expensive
# part. Do not turn a loaded-host timing outlier into another Cargo invocation
# here: mixed gate partitions made that confirmation rebuild the workspace for
# up to five minutes. scripts/slow-test-ratchet.py remains available for
# explicit performance investigations outside the serialized landing gate.
if [ "$gate_skip_fmt" -eq 1 ]; then
    printf 'check.sh: skipping witchy fmt (WITCHY_GATE_SKIP_FMT=1; --full/CI still run it)\n'
else
    run "witchy fmt (std+examples)" witchy_fmt_check
fi
run "prose style (no em dashes)"  prose_em_dash_check

# Product nextest compiles every integration binary before applying its
# filterset, including merge_queue. The shard reuses that artifact once the
# compiler has stopped (arm_queue_overlap above); this stage replays its
# captured output and returns its status without a duplicate workspace build.
[ "$queue_infra" -eq 0 ] || run "queue infrastructure (isolated)" queue_infra_stage

if [ "$gate_skip_rust_class" -eq 1 ]; then
    printf 'check.sh: skipping rust-class (WITCHY_GATE_SKIP_RUST_CLASS=1; --full/CI still run it)\n'
else
    run "Rust-class paired correctness" rust_class_check
fi
if [ "$full" -eq 1 ]; then
    # RFC-0030 bench/soak. The deterministic guards — the never-OOM soak and the
    # per-optimization counter assertions (src/stats.rs) — are HARD-gated in the
    # workspace test step above. The wall-clock benchmarks below are TREND only:
    # timing is noisy and machine-dependent, so a regression warns rather than
    # failing the gate, and the leg is skipped when the harness tools are absent.
    if command -v hyperfine >/dev/null 2>&1 && [ -f bench/run.sh ]; then
        printf '\n\033[1;34m==> [bench] trend vs bench/BASELINE.md (informational)\033[0m\n'
        bash bench/run.sh || printf '\033[1;33mbench: regression or harness issue (non-fatal)\033[0m\n'
    else
        printf '\n\033[1;34m==> [bench] skipped (hyperfine/go toolchain not present)\033[0m\n'
    fi
fi

record_shard_receipt full
printf '\n\033[1;32mall green\033[0m'
[ "$full" -eq 1 ] || printf ' — run with --full for the nightly-deferred tests'
printf '\n'
