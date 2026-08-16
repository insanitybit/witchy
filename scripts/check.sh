#!/usr/bin/env bash
# The green gate: one command that checks the whole workspace, locally. Run it
# before you commit, and before you push. Steps are ordered cheap-to-expensive so
# a failure surfaces as early as possible.
#
#   ./scripts/check.sh --fast  the COMMIT gate: tests (minus the load-flaky
#                              e2e) with clippy overlapped in the background,
#                              plus witchy-fmt IF any .witchy files changed —
#                              the fast inner loop
#   ./scripts/check.sh         the MERGE gate: tests + fmt in the foreground with
#                              a compile check (cargo check), clippy, and the wasm
#                              playground build overlapped in the background
#                              (collected before the gate goes green). Fail-fast:
#                              a background leg that finishes RED aborts the
#                              foreground tests immediately instead of hiding
#                              behind ~20 min of green tests
#   ./scripts/check.sh --full  the PUSH gate: also the e2e suite + from-scratch acceptance
#
# Shards — ONE named section, for a focused pre-queue run in your own worktree.
# A green shard does not replace the full gate; scripts/merge-queue.sh runs that
# once, serialized, at merge time:
#   ./scripts/check.sh --e2e       just the e2e nextest binary (coven/pm/glamour)
#   ./scripts/check.sh --examples  just the example differential matrix (example_tests::*)
#   ./scripts/check.sh --wasm      just the wasm playground build
#   ./scripts/check.sh --queue-infra  merge-queue fixtures, isolated and serial
#   ./scripts/check.sh --glamour   glamour + coven-web tests
#   ./scripts/check.sh --grimoire  grimoire + coven tests
# The last two are UNGATED projects: the merge gate does not run them by
# default, so run the shard yourself while working on one (see the UNGATED
# PROJECTS block below).
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

# The merge coordinator marks serialized gates with WITCHY_GATE_SCOPE. Clear
# Cargo's configured wrapper there so a detached coordinator does not inherit
# a sandbox-incompatible sccache process. The coordinator also exports these
# values after this change lands; keeping the check self-hosting lets this
# candidate pass when an older coordinator is the one gating it.
if [ -n "${WITCHY_GATE_SCOPE+x}" ]; then
    export RUSTC_WRAPPER=
    export CARGO_BUILD_RUSTC_WRAPPER=
    # nextest discovers tests by executing every test binary. macOS otherwise
    # pages large local-symbol tables into memory before any test runs. Strip
    # only serialized-gate test artifacts; developer builds retain symbols and
    # an explicit Cargo profile override remains authoritative.
    if [ -z "${CARGO_PROFILE_TEST_STRIP+x}" ]; then
        export CARGO_PROFILE_TEST_STRIP=symbols
    fi
    # Re-enable incremental compilation in the gate. The coordinator sets
    # CARGO_INCREMENTAL=0 (merge-queue.sh run_gate) — but that existed ONLY
    # because sccache rejects incremental compiles, and sccache is cleared
    # above (it EPERMs in the gate sandbox anyway; see BUG-579). With sccache
    # gone, CARGO_INCREMENTAL=0 is pure waste: the gate worktree persists its
    # target/ across gates and is idle-prewarmed, so incremental lets an
    # unchanged crate be REUSED instead of recompiled. Measured (test profile,
    # 1-line change): incremental rebuild 13s vs CARGO_INCREMENTAL=0 rebuild 72s
    # — ~59s saved per gate. `unset` overrides the coordinator's exported 0
    # without editing run_gate. (Cost: incremental/ grows on disk; the gate
    # target is periodically reclaimable.)
    unset CARGO_INCREMENTAL
fi

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
        --e2e | --examples | --wasm | --queue-infra | --glamour | --grimoire) shard="${arg#--}" ;;
        -h | --help) sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "check.sh: unknown argument '$arg' (try --fast, --full, --e2e, --examples, --wasm, --queue-infra, --glamour, --grimoire, or --help)" >&2; exit 2 ;;
    esac
done
if [ -n "$shard" ] && { [ "$full" -eq 1 ] || [ "$fast" -eq 1 ]; }; then
    echo "check.sh: --$shard cannot be combined with --fast/--full" >&2; exit 2
fi

# Honor a custom CARGO_TARGET_DIR (concurrent agents run with per-agent target
# dirs, e.g. CARGO_TARGET_DIR=target-claude): the cargo build below writes the
# binary there, not under ./target, so the fmt check must look there too.
target_dir="${CARGO_TARGET_DIR:-target}"

# The wasm build needs the rustup toolchain's std: a Homebrew rustc/cargo first on
# PATH can't supply wasm `core`, and `rustup run` alone does not fix it. Force the
# toolchain's bin dir to the front and clear any RUSTC/RUSTFLAGS override (the same
# approach as build-playground.sh).
if command -v rustup >/dev/null; then
    rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
    tc_bin="$(dirname "$(rustup which --toolchain stable rustc)")"
    wasm_cargo=(env -u RUSTC -u RUSTFLAGS "PATH=$tc_bin:$PATH" cargo)
else
    wasm_cargo=(cargo)
fi

# Fuzz policy (WITCHY_GATE_FUZZ, default `full`) — set by the merge-queue
# coordinator from the branch diff; unset here means full, so a standalone run is
# unchanged. The differential fuzzer is the single biggest test (~57s) and a fixed
# 30-seed REGRESSION set (not exploratory), so it earns its keep only when a change
# can affect backend parity:
#   full     30 seeds (default; standalone, --full, and when unsure)
#   reduced  10 seeds — a change touches parity surface (codegen/interp/std/…); the
#            full 30 still run post-merge on CI (heap-checked) and in `--full`
#   skip     none — a docs/rfc/bug/config-only diff cannot change parity, so no
#            fuzz seed could catch anything; skip the whole differential_fuzz binary
# `reduced` works by setting WITCHY_FUZZ_PROGRAMS (only the big test reads it, so
# the cheaper metamorphic checks keep their full counts). `skip` filters the binary.
gate_fuzz="${WITCHY_GATE_FUZZ:-full}"
# `--full` is the EXHAUSTIVE push gate (it adds the heap-check / uaf / rc / type
# sanitizer fuzz sweeps below, several of which run `differential_fuzz` at its
# DEFAULT 30 seeds). Diff-scoping is only for the per-merge default gate — never
# weaken --full — so force `full` here regardless of WITCHY_GATE_FUZZ. Otherwise a
# reduced/skip mode would silently shrink or drop the very sweeps --full exists for.
[ "$full" -eq 1 ] && gate_fuzz="full"
# `reduced` runs the fuzzer under WITCHY_FUZZ_PROGRAMS=N. We set that var in this
# shell's OWN environment (exported below only for that mode) rather than splicing
# an `env` array into test_cmd — bash 3.2 (the macOS gate host) errors on an EMPTY
# array expansion under `set -u`, so an unused `fuzz_env=()` would abort the script.
fuzz_excl=""
case "$gate_fuzz" in
    reduced) export WITCHY_FUZZ_PROGRAMS="${WITCHY_GATE_FUZZ_REDUCED:-10}" ;;
    skip)    fuzz_excl="not binary(differential_fuzz)" ;;
    full)    ;;
    *) echo "check.sh: unknown WITCHY_GATE_FUZZ='$gate_fuzz' (want full|reduced|skip)" >&2; exit 2 ;;
esac

# Gate scope (WITCHY_GATE_SCOPE, default `all`) — set by the merge-queue
# coordinator from the batch diff (see merge-queue.sh). `docs` means every
# changed path is documentation that NO test or gate stage reads (rfcs/ minus
# rfcs/performance-modes.md — which example_tests::
# public_sources_do_not_call_legacy_render_intrinsic reads — wiki/, bugs/,
# and the gitignored scratch//security-eval/):
# such a diff cannot change any gate stage's outcome, so the heavy stages
# would only re-validate the already-gated master tree. The default merge-gate
# mode skips them; post-merge CI still runs the complete suite as the
# backstop. --fast, --full, and the shards ignore the scope entirely (force
# `all`): they are human/agent-invoked runs whose point is to execute their
# section.
gate_scope="${WITCHY_GATE_SCOPE:-all}"
case "$gate_scope" in
    all | docs) ;;
    *) echo "check.sh: unknown WITCHY_GATE_SCOPE='$gate_scope' (want all|docs)" >&2; exit 2 ;;
esac
if [ "$full" -eq 1 ] || [ "$fast" -eq 1 ]; then gate_scope="all"; fi

# UNGATED PROJECTS ----------------------------------------------------------
# glamour, coven-web, grimoire and coven are applications built ON witchy, not
# the language the merge gate exists to protect. Their tests were charging
# every unrelated branch for projects the gate does not guard: together 163 of
# ~2750 tests for ~45% of the whole suite's CPU, roughly two minutes of every
# gate. Breaking one of them while nobody is working on it is an accepted
# outcome, so the DEFAULT here is to skip them all.
#
# WITCHY_GATE_UNGATED is the space-separated list of families to SKIP. The
# merge-queue coordinator overrides it with exactly the families the batch does
# not touch (see merge-queue.sh), so a branch that edits a project still gets
# that project's tests. `--full` clears the list and runs everything, and each
# family has a focused shard (`--glamour`, `--grimoire`) for working on it.
#
# Note `${VAR-default}`, not `${VAR:-default}`: an explicitly EMPTY value means
# "gate everything", which is distinct from the variable being unset.
#
# Each filterset must select the family's whole surface wherever its tests
# physically live, and must stay in step with the owning paths in
# merge-queue.sh. coven's publish flows already live entirely in the `e2e`
# binary, which the default (non-`--full`) gate excludes regardless.
#
# glamour: its own binary, every `witchy web` subcommand test (`::tests::` and
# `::dev::tests::` alike), the glamour example matrix, and the three web CLI
# flows in cli_subcommands. The anchored `web_`/`static_web_` deliberately does
# NOT catch `example_tests::*::webauthn_*`, which is stdlib crypto, not this UI.
ungated_filter_glamour='binary(glamour) or test(/^commands::web::/) or test(/^example_tests::glamour::/) or (binary(cli_subcommands) and test(/^(web_|static_web_)/))'
# grimoire/coven: the package-manager and registry example matrix.
ungated_filter_grimoire='test(/^example_tests::pm_coven::/)'

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

gate_ungated="${WITCHY_GATE_UNGATED-glamour grimoire}"
[ "$full" -eq 1 ] && gate_ungated=""
ungated_excl=""
for family in $gate_ungated; do
    case "$family" in
        glamour)  family_filter="$ungated_filter_glamour" ;;
        grimoire) family_filter="$ungated_filter_grimoire" ;;
        *) echo "check.sh: unknown WITCHY_GATE_UNGATED family '$family' (want glamour|grimoire)" >&2; exit 2 ;;
    esac
    ungated_excl="${ungated_excl:+$ungated_excl and }not ($family_filter)"
done

# Queue fixtures spawn detached coordinators, process groups, lock holders, and
# nested throwaway Git repositories. Running them beside 2,000 product tests
# makes their bounded readiness assertions measure machine contention instead
# of queue behavior. The coordinator enables this shard only when the batch
# touches queue infrastructure; operators can force it for exact-master
# reliability baselines. It runs after product/background work so process and
# timing assertions see an idle compiler and reuse the already-built test binary.
queue_infra="${WITCHY_GATE_QUEUE_INFRA:-0}"
queue_infra_jobs="${WITCHY_QUEUE_INFRA_JOBS:-2}"
case "$queue_infra" in
    0 | 1) ;;
    *) echo "check.sh: WITCHY_GATE_QUEUE_INFRA must be 0 or 1" >&2; exit 2 ;;
esac
case "$queue_infra_jobs" in
    '' | *[!0-9]* | 0)
        echo "check.sh: WITCHY_QUEUE_INFRA_JOBS must be a positive integer" >&2
        exit 2
        ;;
esac
[ "$queue_infra" -eq 0 ] || gate_scope="all"

# Prefer nextest (the project's runner); fall back to plain `cargo test`.
# By default, exclude the load-flaky e2e binary (coven/glamour publish tests)
# from the merge gate. It runs explicitly via `--e2e` and as part of `--full`.
if cargo nextest --version >/dev/null 2>&1; then
    # On macOS the list wrapper avoids a second cold exec per test binary by
    # deriving the ignored list from the ordinary list. Keep that optimization
    # fail-closed: a new/renamed #[ignore] must update its two-name audit before
    # the gate runs, otherwise nextest could misclassify the test.
    scripts/nextest-list-wrapper.sh --validate-ignore-policy "$PWD"
    # Combine the e2e exclusion (default/--fast) and the fuzz-skip exclusion into one
    # filterset (nextest ANDs, so join with ` and `).
    # Queue-infrastructure fixtures run only in their isolated shard below.
    excl="not binary(merge_queue)"
    census_proof_sha="${WITCHY_GATE_CENSUS_PROOF_SHA:-}"
    if [ -n "$census_proof_sha" ] \
        && [ "$(git rev-parse HEAD 2>/dev/null || true)" = "$census_proof_sha" ]; then
        excl="$excl and not test(/^rfc0087_migration_census::repository_census_matches_the_checked_in_type_resolved_snapshot$/)"
        printf 'check.sh: reusing exact RFC-0087 census proof for %.12s\n' "$census_proof_sha"
    fi
    [ "$full" -eq 0 ] && excl="$excl and not binary(e2e)"
    if [ -n "$fuzz_excl" ]; then
        excl="${excl:+$excl and }$fuzz_excl"
    fi
    if [ -n "$ungated_excl" ]; then
        excl="${excl:+$excl and }$ungated_excl"
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
    if [ -n "$gate_test_jobs" ] && [ -n "$excl" ]; then
        test_cmd=(cargo nextest run "$max_fail" -j "$gate_test_jobs" --workspace -E "$excl")
    elif [ -n "$gate_test_jobs" ]; then
        test_cmd=(cargo nextest run "$max_fail" -j "$gate_test_jobs" --workspace)
    elif [ -n "$excl" ]; then
        test_cmd=(cargo nextest run "$max_fail" --workspace -E "$excl")
    else
        test_cmd=(cargo nextest run "$max_fail" --workspace)
    fi
    # Every queue fixture owns a distinct temporary repository, state root, and
    # process group. Width two removes the serialized 31-fixture floor while
    # retaining an operator override for repeated reliability measurements.
    queue_infra_cmd=(cargo nextest run --test merge_queue -j "$queue_infra_jobs")
else
    # Plain cargo test runs integration binaries sequentially, so the queue
    # fixture is already isolated from other binaries in this fallback mode.
    test_cmd=(cargo test --workspace)
    queue_infra_cmd=(cargo test --test merge_queue -- --test-threads=1)
fi

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
    "$target_dir/debug/witchy" fmt --check "${files[@]}"
}

# Run pinned examples and every complete book example through the SHIPPED
# browser wasm + host. The second audit uses the exact providers wired into the
# live docs page, so a browser-runnable classification cannot create a false Run
# button. Uses the wasm built in the step above. Best-effort:
# skips (green) if node or the wasm are absent so the gate never hard-depends on
# a JS toolchain that may not be present everywhere.
validate_runnable_book() {
    command -v node >/dev/null 2>&1 || { echo "node absent — skipping runnable-book validation"; return 0; }
    [ -f scripts/validate_book_examples.mjs ] || { echo "validator absent — skipping"; return 0; }
    local built="$target_dir/wasm32-unknown-unknown/debug/witchy.wasm"
    [ -f "$built" ] || { echo "wasm not built at $built — skipping"; return 0; }
    local -a node_flags=()
    if node --v8-options 2>/dev/null | grep -q -- '--experimental-wasm-jspi'; then
        node_flags+=(--experimental-wasm-jspi)
    fi
    if ! node "${node_flags[@]+"${node_flags[@]}"}" -e \
        'process.exit(typeof WebAssembly.Suspending === "function" && typeof WebAssembly.promising === "function" ? 0 : 1)'; then
        echo "Node lacks WebAssembly JSPI — skipping runnable-book validation"
        return 0
    fi
    # Point the validator at the freshly-built wasm via env — do NOT copy over the
    # tracked web/witchy.wasm (a dirtied worktree would break the coordinator's ff-merge).
    WITCHY_WASM_PATH="$built" node "${node_flags[@]+"${node_flags[@]}"}" scripts/validate_book_examples.mjs
    WITCHY_WASM_PATH="$built" node "${node_flags[@]+"${node_flags[@]}"}" scripts/audit-browser-runnable.mjs
    WITCHY_WASM_PATH="$built" node "${node_flags[@]+"${node_flags[@]}"}" web/witchy-runtime/fixture-host.test.mjs
}

# RFC-0111's ordinary gate leg is deliberately untimed: it proves that every
# paired Witchy/Rust case agrees with its independent result. On ARM it also
# proves that the Rust translation units contain no packed-vector instructions;
# other supported gate hosts report correctness-only rather than claiming
# scalar evidence. Build the Witchy executable with the exact candidate commit
# embedded, then make the harness authenticate that identity before it accepts
# the results.
rust_class_check() {
    local head witchy_bin rust_build_dir
    head=$(git rev-parse --verify HEAD)
    WITCHY_BUILD_COMMIT="$head" cargo build -p witchy --quiet
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
        e2e)
            cargo nextest --version >/dev/null 2>&1 || { echo "check.sh: --e2e requires cargo-nextest" >&2; exit 2; }
            cargo nextest run --workspace -E 'binary(e2e)'
            ;;
        examples)
            cargo nextest --version >/dev/null 2>&1 || { echo "check.sh: --examples requires cargo-nextest" >&2; exit 2; }
            cargo nextest run --workspace -E 'test(/^example_tests::/)'
            ;;
        wasm)
            "${wasm_cargo[@]}" build --lib --no-default-features --features browser-fixtures --target wasm32-unknown-unknown
            # Same browser-runnability check the full gate runs — so an agent
            # validating a book/classifier change with `--wasm` catches a false
            # Run button in their focused shard, not only at full-gate time.
            validate_runnable_book
            ;;
        queue-infra)
            cargo nextest --version >/dev/null 2>&1 || { echo "check.sh: --queue-infra requires cargo-nextest" >&2; exit 2; }
            "${queue_infra_cmd[@]}"
            ;;
        glamour)
            # The tests the merge gate no longer runs by default. Run this while
            # working on glamour or coven-web — the queue will not catch a
            # regression there for you.
            cargo nextest --version >/dev/null 2>&1 || { echo "check.sh: --glamour requires cargo-nextest" >&2; exit 2; }
            cargo nextest run --workspace -E "$ungated_filter_glamour"
            ;;
        grimoire)
            # Likewise for grimoire/coven. Run this while working on the package
            # manager or the registry; the queue will not catch it for you.
            cargo nextest --version >/dev/null 2>&1 || { echo "check.sh: --grimoire requires cargo-nextest" >&2; exit 2; }
            cargo nextest run --workspace -E "$ungated_filter_grimoire"
            ;;
    esac
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

# Clippy runs as a BACKGROUND leg in both the fast and the full gate: lint
# artifacts (wrapper-generated rmeta) share nothing with build/test artifacts,
# so it checks in its OWN target dir (CoW-cloned from the main one on first
# use, warm thereafter) instead of serializing ~60-75s (warm; minutes under
# contention) in front of the tests. Same-dir invocations would serialize on
# cargo's build lock. A leg failure still fails the gate — via run_watched's
# fail-fast while the tests run (the foreground stage is aborted the moment a
# leg reports red), or at collect time at the latest.
# Conservative clippy policy (shared by the gate, the coordinator prewarm, and
# CI). We deny only the GENUINE-BUG tiers — clippy's `correctness` group (deny by
# default; code that is outright wrong or useless), `suspicious` (most likely
# incorrect), and `unused_must_use` (a dropped `Result`/`#[must_use]`, a real bug
# in a capability language) — and DELIBERATELY do NOT pass `-D warnings`. Style,
# complexity, perf, and pedantic lints (e.g. `needless_borrow`) still print, but
# they never block a merge: a needless borrow is not a reason to redo a gate.
# Keep this list identical everywhere so the CoW-seeded clippy target cache is
# reused instead of invalidated by a differing lint set.
CLIPPY_GATE_LINTS=(-D clippy::correctness -D clippy::suspicious -D unused_must_use)
clippy_dir="${target_dir}-clippy"
seed_cow_target() { # seed_cow_target <dir>
    # `mkdir` is the atomic claim: exactly one concurrent run seeds; any other
    # sees the dir and uses it as-is (a dir left partially seeded by a killed
    # cp is safe — cargo treats missing artifacts as a cold cache, so the dir
    # self-heals on the next run). APFS clonefile (`cp -c`) or reflink where
    # available: seconds, ~zero disk. Where neither works, the dir stays
    # empty — a one-time cold run.
    local dir="$1"
    [ -d "$dir" ] && return 0
    mkdir "$dir" 2>/dev/null || return 0
    if [ -d "$target_dir" ]; then
        cp -Rc "$target_dir/." "$dir/" 2>/dev/null \
            || cp -R --reflink=auto "$target_dir/." "$dir/" 2>/dev/null \
            || true
    fi
}
launch_clippy_leg() {
    clippy_log="$(mktemp "${TMPDIR:-/tmp}/witchy-clippy-XXXXXX")"
    clippy_started=$(date +%s)
    ( seed_cow_target "$clippy_dir" && background_leg env CARGO_TARGET_DIR="$clippy_dir" cargo clippy --workspace --all-targets -- "${CLIPPY_GATE_LINTS[@]}" ) >"$clippy_log" 2>&1 &
    clippy_pid=$!
}

# The compile check is a third background leg in the MERGE gate only (not
# --fast, whose contract is pinned to two legs): `cargo check --workspace
# --all-targets` is the cheapest command that surfaces a plain rustc compile
# error in ANY target (warm: 1-3 min), long before nextest's own test-profile
# build reports it (observed t+1004s in a red gate). Combined with
# run_watched's fail-fast, a compile error now kills the gate in minutes.
# Like clippy it runs in its own CoW-seeded target dir so it never serializes
# on cargo's build lock against the tests or the clippy leg.
check_dir="${target_dir}-check"
launch_check_leg() {
    check_log="$(mktemp "${TMPDIR:-/tmp}/witchy-check-XXXXXX")"
    check_started=$(date +%s)
    ( seed_cow_target "$check_dir" && background_leg env CARGO_TARGET_DIR="$check_dir" cargo check --workspace --all-targets ) >"$check_log" 2>&1 &
    check_pid=$!
}

# Preserve the command's real finish time without a sidecar file. The marker is
# written into the leg's existing private log and stripped before diagnostics;
# collect_bg turns it into the ordinary WITCHY_TIMING record. The wrapper owns
# and forwards TERM/INT to the command so reap_bg retains the old `exec` safety:
# abandoning a red foreground stage cannot orphan cargo behind the next gate.
background_leg() { # background_leg <command...>
    local child="" command_status=0 finished
    trap 'kill "${child:-}" 2>/dev/null || true; wait "${child:-}" 2>/dev/null || true; exit 143' TERM INT
    "$@" &
    child=$!
    wait "$child" || command_status=$?
    trap - TERM INT
    finished=$(date +%s)
    # The marker carries finish time AND exit status: run_watched's fail-fast
    # scan reads the status from the log (never `wait`), so collect_bg still
    # owns the pid's single wait.
    printf 'WITCHY_BG_FINISHED %d %d\n' "$finished" "$command_status"
    return "$command_status"
}

# Has a background leg already finished RED? Judged from its log marker only —
# no `wait`, no pid liveness games — so a green leg is still collected (and
# reported) exactly as before by collect_bg.
leg_finished_red() { # leg_finished_red <log>
    local status
    [ -n "${1:-}" ] && [ -f "$1" ] || return 1
    status="$(sed -n 's/^WITCHY_BG_FINISHED [0-9][0-9]* \([0-9][0-9]*\)$/\1/p' "$1" | tail -1)"
    [ -n "$status" ] && [ "$status" -ne 0 ]
}

# Scan every launched leg for a recorded failure; on the first hit, publish it
# via failfast_* and clear its pid variable (collect ownership moves to the
# abort path, and reap_bg must not signal a pid another wait will reap).
failfast_scan() {
    if [ -n "${check_pid:-}" ] && leg_finished_red "${check_log:-}"; then
        failfast_label="compile check (cargo check)"; failfast_pid="$check_pid"
        failfast_log="$check_log"; failfast_started="$check_started"
        check_pid=""
        return 0
    fi
    if [ -n "${clippy_pid:-}" ] && leg_finished_red "${clippy_log:-}"; then
        failfast_label="clippy (bug lints)"; failfast_pid="$clippy_pid"
        failfast_log="$clippy_log"; failfast_started="$clippy_started"
        clippy_pid=""
        return 0
    fi
    if [ -n "${wasm_pid:-}" ] && leg_finished_red "${wasm_log:-}"; then
        failfast_label="wasm playground build"; failfast_pid="$wasm_pid"
        failfast_log="$wasm_log"; failfast_started="$wasm_started"
        wasm_pid=""
        return 0
    fi
    return 1
}

# Reap the background legs on ANY exit — green, red at tests/fmt, or a failed
# collect. background_leg forwards the signal to its cargo child (rustc
# children sit in the surrounding process group, which the coordinator's
# timeout kill already covers). Without this, a red foreground stage abandoned live cargo
# processes that kept building while the coordinator rebased the next
# candidate in the same worktree. collect_bg clears each pid after reaping
# it: a waited-on pid may be recycled by the OS and must never be signalled.
# The trap is installed before any leg launches; every guard tolerates
# nothing-launched-yet, so modes that skip the legs exit through it safely.
reap_bg() {
    [ -n "${stage_heartbeat_pid:-}" ] && kill "$stage_heartbeat_pid" 2>/dev/null || true
    [ -n "${failfast_fg_pid:-}" ] && kill "$failfast_fg_pid" 2>/dev/null || true
    [ -n "${check_pid:-}" ] && kill "$check_pid" 2>/dev/null || true
    [ -n "${clippy_pid:-}" ] && kill "$clippy_pid" 2>/dev/null || true
    [ -n "${wasm_pid:-}" ] && kill "$wasm_pid" 2>/dev/null || true
    [ -n "${check_log:-}" ] && rm -f "$check_log" || true
    [ -n "${clippy_log:-}" ] && rm -f "$clippy_log" || true
    [ -n "${wasm_log:-}" ] && rm -f "$wasm_log" || true
    return 0
}
trap reap_bg EXIT
trap 'exit 143' TERM INT

# Collect a background leg, cheapest-to-diagnose first. On failure, show the
# leg's full output and exit red.
collect_bg() { # collect_bg <label> <pid> <log> <started>
    local label="$1" pid="$2" log="$3" started="$4"
    step=$((step + 1))
    local t_collect; t_collect=$(date +%s)
    printf '\n\033[1;34m==> [%d] %s (t+%ds)\033[0m\n' "$step" "$label" "$(( t_collect - t_start ))"
    local command_status=0
    wait "$pid" || command_status=$?
    local t_finished
    t_finished=$(sed -n 's/^WITCHY_BG_FINISHED \([0-9][0-9]*\).*$/\1/p' "$log" | tail -1)
    case "$t_finished" in '' | *[!0-9]*) t_finished=$(date +%s) ;; esac
    if [ "$command_status" -ne 0 ]; then
        emit_timing background "$step" "$label" red "$started" "$t_finished"
        sed '/^WITCHY_BG_FINISHED /d' "$log"
        rm -f "$log"
        printf '\033[1;31m%s FAILED\033[0m\n' "$label"
        exit 1
    fi
    emit_timing background "$step" "$label" green "$started" "$t_finished"
    rm -f "$log"
    printf '\033[1;34m    [%d] %s took %ds\033[0m\n' "$step" "$label" "$(( t_finished - started ))"
}

# Like `run`, but FAIL-FAST: while the foreground stage executes, poll the
# background legs' logs; the moment one records a failure, abort the foreground
# stage, surface the red leg's full output via collect_bg, and exit red. This
# is what turns a 1-3 min clippy/compile failure into a ~2-4 min red gate
# instead of one that only surfaces after ~20 min of doomed tests. On the
# all-green path it is byte-identical to `run` (same marker, heartbeat, and
# WITCHY_TIMING record; the poll adds at most WITCHY_FAILFAST_POLL seconds of
# latency after the stage finishes). The aborted foreground stage is emitted
# with status "aborted" — consumers that only read "green" (gate-report.sh)
# ignore it, and the red leg's own record carries the failure.
run_watched() {
    step=$((step + 1))
    local t_stage; t_stage=$(date +%s)
    printf '\n\033[1;34m==> [%d] %s (t+%ds)\033[0m\n' "$step" "$1" "$(( t_stage - t_start ))"
    local label="$1"
    shift
    stage_heartbeat "$step" "$label" "$t_stage" &
    stage_heartbeat_pid=$!
    "$@" &
    failfast_fg_pid=$!
    local poll="${WITCHY_FAILFAST_POLL:-5}"
    case "$poll" in '' | *[!0-9]* | 0) poll=5 ;; esac
    while kill -0 "$failfast_fg_pid" 2>/dev/null; do
        if failfast_scan; then
            printf '\n\033[1;31m%s FAILED while %s ran — aborting the foreground stage (fail-fast)\033[0m\n' \
                "$failfast_label" "$label"
            kill "$failfast_fg_pid" 2>/dev/null || true
            wait "$failfast_fg_pid" 2>/dev/null || true
            failfast_fg_pid=""
            kill "$stage_heartbeat_pid" 2>/dev/null || true
            wait "$stage_heartbeat_pid" 2>/dev/null || true
            stage_heartbeat_pid=""
            emit_timing foreground "$step" "$label" aborted "$t_stage" "$(date +%s)"
            # collect_bg prints the red leg's output, emits its red timing
            # record, and exits 1 (the EXIT trap reaps the remaining legs).
            collect_bg "$failfast_label" "$failfast_pid" "$failfast_log" "$failfast_started"
            exit 1
        fi
        sleep "$poll"
    done
    local command_status=0
    wait "$failfast_fg_pid" || command_status=$?
    failfast_fg_pid=""
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
    # The fast commit gate: tests in the foreground, clippy overlapped behind
    # them (collected — and able to fail the gate — before green). nextest
    # compiles+links its own test binaries, so no standalone build step.
    launch_clippy_leg
    # Run the witchy fmt check IF any .witchy files are dirty — catches formatting
    # mistakes without the cost of always building the binary + sweeping 200 files.
    # (This build is FOREGROUND in the MAIN target dir — no clippy-leg overlap
    # concerns; nextest below reuses its artifacts.)
    if git diff --name-only HEAD 2>/dev/null | grep -q '\.witchy$'; then
        run "build (binary)"           cargo build -p witchy
        run "witchy fmt (changed .witchy files)" witchy_fmt_check
    fi
    if git diff --name-only HEAD 2>/dev/null | grep -qE '\.(md|witchy)$'; then
        run "prose style (no em dashes)" prose_em_dash_check
    fi
    run_watched "tests (workspace, minus e2e)" "${test_cmd[@]}"
    collect_bg "clippy (bug lints)" "$clippy_pid" "$clippy_log" "$clippy_started"
    clippy_pid=""
    # Product discovery already builds the excluded merge_queue test binary.
    # Run its hermetic shard after all product work so it reuses that artifact
    # and no background compiler load can distort process/timing assertions.
    [ "$queue_infra" -eq 0 ] || run "queue infrastructure (isolated)" "${queue_infra_cmd[@]}"
    printf '\n\033[1;32mfast gate green\033[0m — run without --fast before push (fmt + wasm), --full for e2e\n'
    exit 0
fi

if [ "$gate_scope" = "docs" ]; then
    run "docs-only scope — heavy stages skipped" true
    printf '\n\033[1;32mall green\033[0m — WITCHY_GATE_SCOPE=docs: the batch diff touches only untested documentation; tests/clippy/fmt/wasm/book skipped (post-merge CI runs the full suite)\n'
    exit 0
fi

# The full gate runs four independent legs CONCURRENTLY and collects the
# background three after the foreground one finishes:
#   [fg] tests  — nextest builds the workspace itself, including a fresh `witchy`
#                 binary: the integration tests name it via CARGO_BIN_EXE_witchy,
#                 so cargo must (re)build the bin before compiling them. That
#                 makes a standalone pre-build redundant, and the fmt check below
#                 can run AFTER the tests against the nextest-built binary.
#   [bg] check  — see launch_check_leg above (own CoW-cloned target dir);
#                 surfaces plain compile errors in minutes.
#   [bg] clippy — see launch_clippy_leg above (own CoW-cloned target dir).
#   [bg] wasm   — targets wasm32-unknown-unknown, shares no native artifacts
#                 (this leg overlapped tests before this restructure too).
# A background-leg failure fails the gate FAST: run_watched polls the legs
# while the tests run and aborts them the moment a leg records a failure, so
# a red check/clippy/wasm surfaces in minutes instead of after the full test
# stage. The all-green path is unchanged (overlap, not serialization).
launch_check_leg
launch_clippy_leg

wasm_log="$(mktemp "${TMPDIR:-/tmp}/witchy-wasm-XXXXXX")"
wasm_started=$(date +%s)
( background_leg "${wasm_cargo[@]}" build --lib --no-default-features --features browser-fixtures --target wasm32-unknown-unknown ) >"$wasm_log" 2>&1 &
wasm_pid=$!

run_watched "tests (workspace)"        "${test_cmd[@]}"
run "witchy fmt (std+examples)" witchy_fmt_check
run "prose style (no em dashes)"  prose_em_dash_check

collect_bg "compile check (cargo check)" "$check_pid" "$check_log" "$check_started"
check_pid=""
collect_bg "clippy (bug lints)"   "$clippy_pid" "$clippy_log" "$clippy_started"
clippy_pid=""
collect_bg "wasm playground build"    "$wasm_pid"   "$wasm_log" "$wasm_started"
wasm_pid=""

# Product nextest compiles every integration binary before applying its
# filterset, including merge_queue. Reuse that artifact only after every
# compiler leg has stopped, keeping fixture timing isolated without paying a
# duplicate workspace build at the front of the gate.
[ "$queue_infra" -eq 0 ] || run "queue infrastructure (isolated)" "${queue_infra_cmd[@]}"

run "runnable book (browser)"  validate_runnable_book
run "Rust-class paired correctness" rust_class_check
if [ "$full" -eq 1 ]; then
    # RFC-0023 memory-safety sweep: re-run the differential fuzzer with the checked
    # heap on, so a codegen heap bug (wrong offset, missing ensure, mis-layout) in
    # any optimization surfaces as a redzone trap or backend DIVERGE on random
    # programs — not just the curated suite. Runs with all WITCHY_OPT levers on.
    run "fuzz (checked heap)"  env WITCHY_HEAP_CHECK=1 cargo nextest run --test differential_fuzz
    # RFC-0037 §3 use-after-free sweep: the sanitizer poisons every freed block, so a
    # use-after-free of an un-reused block reads a trap pattern deterministically (a class
    # the freelist-clobber-based differential can miss). On a correct compiler this is
    # output-preserving, so a DIVERGE here is a real reclamation bug under `rc-floor`. Wider
    # than the in-suite default so more freed-then-read shapes are exercised.
    run "fuzz (uaf sanitizer)" env WITCHY_UAF_FUZZ_PROGRAMS=40 cargo nextest run --test differential_fuzz -E 'test(uaf_sanitizer)'
    # RFC-0051 I1 dup/drop assertion sweep: WITCHY_RC_ASSERT traps (fire-and-report) when a
    # value with an implausible header reaches $rc_dup/$rc_drop — an I1 emission-invariant
    # violation (a view/slice/scalar dup'd/dropped). Zero fires across this + examples + e2e is
    # the RFC's precondition for deleting the release-path plausibility heuristic. A DIVERGE
    # here names a real type-predicate gap (the SEC-037 class), not a false positive.
    run "fuzz (rc assertion)"  env WITCHY_RC_ASSERT_PROGRAMS=40 cargo nextest run --test differential_fuzz -E 'test(rc_assert_dup_drop_is_false_positive_free)'
    # RFC-0037 §3 type-confusion sweep: WITCHY_TYPE_CHECK tags every $rc_alloc object and asserts
    # the tag at typed reads (boxed .field + packed unbox at().field). On a correct compiler it is
    # output-preserving, so a DIVERGE/trap is a real layout/unbox confusion. Runs the whole fuzz
    # corpus (records/ADTs/packable lists) under the sanitizer.
    run "fuzz (type sanitizer)" env WITCHY_TYPE_CHECK=1 cargo nextest run --test differential_fuzz
    run "e2e (from scratch)"   ./scripts/e2e-full.sh
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

printf '\n\033[1;32mall green\033[0m'
[ "$full" -eq 1 ] || printf ' — run with --full for the e2e acceptance test'
printf '\n'
