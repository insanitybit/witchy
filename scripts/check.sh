#!/usr/bin/env bash
# The green gate: one command that checks the whole workspace, locally. Run it
# before you commit, and before you push. Steps are ordered cheap-to-expensive so
# a failure surfaces as early as possible.
#
#   ./scripts/check.sh --fast  the COMMIT gate: build + clippy + tests (minus the
#                              load-flaky e2e), skipping the witchy-fmt and wasm
#                              playground steps — the fast inner loop
#   ./scripts/check.sh         build, clippy, fmt, tests, and the wasm playground build
#   ./scripts/check.sh --full  the PUSH gate: also the e2e suite + from-scratch acceptance
#
# Shards — ONE named section, for a focused pre-queue run in your own worktree.
# A green shard does not replace the full gate; scripts/merge-queue.sh runs that
# once, serialized, at merge time:
#   ./scripts/check.sh --e2e       just the e2e nextest binary (coven/pm/glamour)
#   ./scripts/check.sh --examples  just the example differential matrix (example_tests::*)
#   ./scripts/check.sh --wasm      just the wasm playground build
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

full=0
fast=0
shard=""
for arg in "$@"; do
    case "$arg" in
        --full) full=1 ;;
        --fast) fast=1 ;;
        --e2e | --examples | --wasm) shard="${arg#--}" ;;
        -h | --help) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "check.sh: unknown argument '$arg' (try --fast, --full, --e2e, --examples, --wasm, or --help)" >&2; exit 2 ;;
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

# Prefer nextest (the project's runner); fall back to plain `cargo test`.
# In --fast mode, exclude the load-flaky e2e binary (coven/glamour publish tests)
# from the run — it is the push gate's job, not the per-commit loop's.
if cargo nextest --version >/dev/null 2>&1; then
    if [ "$fast" -eq 1 ]; then
        test_cmd=(cargo nextest run --workspace -E 'not binary(e2e)')
    else
        test_cmd=(cargo nextest run --workspace)
    fi
else
    test_cmd=(cargo test --workspace)
fi

# A named shard runs exactly one section and exits (reporting elapsed time).
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
            "${wasm_cargo[@]}" build --lib --no-default-features --target wasm32-unknown-unknown
            ;;
    esac
    printf '\n\033[1;32mshard %s green\033[0m in %ds\n' "$shard" "$(( $(date +%s) - shard_t0 ))"
    exit 0
fi

# The witchy formatter over the stdlib and examples (NOT rustfmt over the Rust —
# that's hand-formatted and out of scope). Uses the binary built in step 1.
# ONE invocation over every file, not one process per file: `witchy fmt --check`
# already processes all path args, names each unformatted file on stderr, and
# exits 1 iff any fails — so the per-file loop only paid ~200 extra process
# spawns (1.5s vs 0.13s over the 205 files) for identical diagnostics.
witchy_fmt_check() {
    local files=()
    local f
    for f in std/*.witchy examples/*/src/*.witchy; do
        [ -f "$f" ] && files+=("$f")
    done
    [ "${#files[@]}" -eq 0 ] && return 0
    "$target_dir/debug/witchy" fmt --check "${files[@]}"
}

# Each stage marker carries its offset from gate start (`==> [2] clippy (t+41s)`),
# so a redirected log is enough to see what is running now (merge-queue.sh
# status/doctor parse these markers), and each stage reports its own duration
# on completion so slow regressions are visible per stage, not just in total.
step=0
t_start=$(date +%s)
run() {
    step=$((step + 1))
    local t_stage; t_stage=$(date +%s)
    printf '\n\033[1;34m==> [%d] %s (t+%ds)\033[0m\n' "$step" "$1" "$(( t_stage - t_start ))"
    local label="$1"
    shift
    "$@"
    printf '\033[1;34m    [%d] %s took %ds\033[0m\n' "$step" "$label" "$(( $(date +%s) - t_stage ))"
}

run "build (workspace)"        cargo build --workspace
run "clippy (deny warnings)"   cargo clippy --workspace --all-targets -- -D warnings
if [ "$fast" -eq 1 ]; then
    # The fast commit gate: tests minus the flaky e2e; skip the witchy-fmt sweep
    # (run it only when .witchy files changed) and the separate wasm compile.
    run "tests (workspace, minus e2e)" "${test_cmd[@]}"
    printf '\n\033[1;32mfast gate green\033[0m — run without --fast before push (fmt + wasm), --full for e2e\n'
    exit 0
fi
run "witchy fmt (std+examples)" witchy_fmt_check
run "tests (workspace)"        "${test_cmd[@]}"
run "wasm playground build"    "${wasm_cargo[@]}" build --lib --no-default-features --target wasm32-unknown-unknown
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
