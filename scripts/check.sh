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
# rustfmt is deliberately NOT part of the gate: the Rust in this repo is
# hand-formatted, so `cargo fmt` would fight the intended style.
set -euo pipefail
cd "$(dirname "$0")/.."

full=0
fast=0
for arg in "$@"; do
    case "$arg" in
        --full) full=1 ;;
        --fast) fast=1 ;;
        -h | --help) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "check.sh: unknown argument '$arg' (try --fast, --full, or --help)" >&2; exit 2 ;;
    esac
done

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

# The witchy formatter over the stdlib and examples (NOT rustfmt over the Rust —
# that's hand-formatted and out of scope). Uses the binary built in step 1.
witchy_fmt_check() {
    local fail=0
    for f in std/*.witchy examples/*/src/*.witchy; do
        [ -f "$f" ] || continue
        target/debug/witchy fmt --check "$f" >/dev/null 2>&1 || { echo "needs formatting: $f"; fail=1; }
    done
    return "$fail"
}

step=0
run() {
    step=$((step + 1))
    printf '\n\033[1;34m==> [%d] %s\033[0m\n' "$step" "$1"
    shift
    "$@"
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
