# justfile — task runner for the witchy project.
# Recipes mirror the CI jobs (.github/workflows/ci.yml), the helper scripts
# under scripts/, and the `witchy` CLI subcommands. Run `just` for the list.

set shell := ["bash", "-uc"]

# The release binary the parity/fmt recipes drive.
bin := "./target/release/witchy"

# Show the available recipes (default when you run bare `just`).
default:
    @just --list

# --- Rust: build / lint / test -------------------------------------------

# Debug build of the `witchy` CLI.
build:
    cargo build

# Release build (used by the parity sweep, fmt check, scripts).
build-release:
    cargo build --release

# Build everything CI builds (all bins, tests, examples).
build-all:
    cargo build --workspace --all-targets

# Workspace unit + integration tests. Extra arguments are passed to nextest.
test *ARGS:
    cargo nextest run --workspace {{ARGS}}

# Capture build, test-compilation, and optional test timings without sharing
# Cargo's default target directory. Use `just metrics --with-tests` for the
# complete local cycle.
metrics *ARGS:
    ./scripts/build-test-metrics.sh {{ARGS}}

# Summarize local timings and recent merge-queue throughput.
perf-health *ARGS:
    ./scripts/perf-health.sh {{ARGS}}

# Run the narrow agent-owned validation shard selected by the caller.
agent-check *ARGS:
    ./scripts/agent-check.sh {{ARGS}}

# Fast source-structure hotspot report; does not invoke Cargo.
structure-health:
    ./scripts/structure-health.sh

# The exact nextest invocation used by CI's test job.
test-ci:
    cargo nextest run --workspace --profile ci --all-targets

# Checked-heap differential fuzz (CI `heap-check` job, RFC-0023): out-of-object
# writes surface as redzone traps / a shadow-sweep failure.
heap-check:
    WITCHY_HEAP_CHECK=1 cargo nextest run --test differential_fuzz

# Lint gate — CI runs this with -D warnings.
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

alias lint := clippy

# Vendored zizmor workflow security audit (CI `zizmor` job, pedantic persona).
zizmor:
    ./scripts/zizmor.sh --quiet --no-progress --persona=pedantic .github/workflows

# NOTE: there is deliberately NO `cargo fmt` recipe. The Rust in this repo is
# hand-formatted on purpose (see scripts/check.sh); `cargo fmt` reformats ~71
# files and fights the intended style. The only formatting gate is `witchy fmt`
# over std/, examples/, and projects/ — see `wfmt` / `wfmt-check` below.

# --- witchy CLI passthroughs ---------------------------------------------

# Run a witchy program: `just run path/to/prog.witchy [args...]`.
run FILE *ARGS: build-release
    {{bin}} {{FILE}} {{ARGS}}

# Type-check a program without running it.
check FILE: build-release
    {{bin}} check {{FILE}}

# Run a program on both backends and confirm identical output.
parity FILE: build-release
    {{bin}} parity {{FILE}}

# Run in-language tests (`test_*` functions) in a file or directory.
witchy-test TARGET: build-release
    {{bin}} test {{TARGET}}

# Report the capability footprint of a program.
caps FILE: build-release
    {{bin}} caps {{FILE}}

# Compile and run a program in a VM granted exactly its footprint.
sandbox FILE *ARGS: build-release
    {{bin}} sandbox {{FILE}} {{ARGS}}

# Run the language server (stdio) — used by editor extensions.
lsp:
    cargo run -- lsp

# --- Parity sweep & witchy formatting (mirror CI) ------------------------

# Differential-check every runnable example on both backends (CI parity job).
# FAIL-CLOSED on any non-zero parity exit (RFC-0058 §2, BUG-002): 0 = agree /
# both-error-agree, 2 = unexpected-error (a compile/lower regression), 3 = diverge.
# No `|| true`, no `grep DIVERGE` — classify on the exit code + the machine-readable
# `parity-stats` line. Vacuity guard: assert we compared > 0 output lines. Positive
# control: the seeded-divergence lever MUST fail parity, proving the gate can fail.
parity-sweep: build-release
    #!/usr/bin/env bash
    set -uo pipefail
    fail=0
    files=0
    compared=0
    ctrl=""
    for f in examples/*/src/*.witchy; do
        # Discover RUNNABLE programs only: `parity` needs a `main`. Libraries and
        # in-language `*_test.witchy` suites (no `main`) are covered by `witchy test`
        # and example_tests — sweeping them would fail-closed on a legitimate
        # "no main to run" (a reviewed skiplist rule, BUG-002 / RFC-0058 §4).
        grep -q 'fn main' "$f" || continue
        [ -z "$ctrl" ] && ctrl="$f"
        files=$((files + 1))
        out=$({{bin}} parity "$f" 2>&1)
        code=$?
        n=$(printf '%s\n' "$out" | grep '^parity-stats ' | tail -1 | sed -n 's/.*compared=\([0-9][0-9]*\).*/\1/p')
        compared=$((compared + ${n:-0}))
        if [ "$code" -ne 0 ]; then
            echo "PARITY FAIL (exit $code): $f"
            echo "$out"
            fail=1
        fi
    done
    if [ "$files" -eq 0 ]; then
        echo "parity-sweep VACUOUS: no example files discovered (BUG-002)" >&2
        exit 1
    fi
    if [ "$compared" -eq 0 ]; then
        echo "parity-sweep VACUOUS: compared 0 output lines across $files files (BUG-002)" >&2
        exit 1
    fi
    # Positive control (RFC-0058 §1): `ctrl` is a known-AGREEING runnable example
    # (it passed above with exit 0). With the seeded-divergence lever armed, parity
    # MUST now fail — a self-test that the gate can still detect a divergence.
    if WITCHY_SEEDED_DIVERGENCE=1 {{bin}} parity "$ctrl" >/dev/null 2>&1; then
        echo "parity-sweep POSITIVE CONTROL FAILED: seeded divergence did NOT fail parity on $ctrl" >&2
        exit 1
    fi
    echo "parity-sweep: $files files, $compared compared lines, positive control OK"
    exit $fail

# Format every std, example, and project witchy source in place.
wfmt: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    files=(std/*.witchy examples/*/src/*.witchy)
    while IFS= read -r f; do
        files+=("$f")
    done < <(find projects -type f -path '*/src/*.witchy' -print | sort)
    {{bin}} fmt "${files[@]}"

# Verify std, example, and project formatting without writing (CI fmt job).
wfmt-check: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    files=(std/*.witchy examples/*/src/*.witchy)
    while IFS= read -r f; do
        files+=("$f")
    done < <(find projects -type f -path '*/src/*.witchy' -print | sort)
    {{bin}} fmt --check "${files[@]}"

# Regenerate the stdlib API reference (a test asserts it stays current).
doc-std: build-release
    {{bin}} doc std/*.witchy > spec/stdlib.md

# --- End-to-end / acceptance ---------------------------------------------

# Full from-scratch acceptance run (builds from scratch, ~30 checks).
e2e:
    ./scripts/e2e-full.sh

# Acceptance run without the `cargo test` stage (CI runs that separately).
e2e-quick:
    ./scripts/e2e-full.sh --quick

# --- Book & playground ----------------------------------------------------

# Build "The witchy Book" — the RFC-0041 docs bundle (glamour app + book content
# compiled to a set of static files) into ./dist, explicitly permitting an
# absent browser compiler: the Run cells show a clear "missing compiler" error.
# Use `just docs-build` (or `book-serve`) for a complete runnable bundle.
book: build-release
    ./scripts/build-docs.sh --allow-missing-compiler dist

# Build the COMPLETE runnable bundle (fresh browser compiler included), then
# serve it locally at http://localhost:8000 — Run cells and /witchy.wasm work.
book-serve: docs-build
    python3 -m http.server -d dist 8000

# Build the in-browser playground (wasm) into web/.
playground:
    ./scripts/build-playground.sh

# Validate every runnable book block against the manifest oracle (the CI
# `playground` job's second step) — needs the browser wasm from `playground`.
book-validate: playground
    node scripts/validate_book_examples.mjs

# Build the deployable docs bundle into ./dist (CI `docs-build` job). The bundle
# builder generates its browser compiler from the same checkout.
docs-build: build-release
    ./scripts/build-docs.sh dist

# Build just the wasm interpreter the way CI does (no native deps).
wasm:
    cargo build --release --lib --no-default-features --target wasm32-unknown-unknown

# --- Aggregates -----------------------------------------------------------

# The full local gate — mirrors every ci.yml job EXCEPT the master-only Pages
# deploy (`docs-deploy`), which needs GitHub Pages/OIDC and isn't locally
# reproducible. See .github/workflows/ci.yml for the authoritative set.
#   zizmor        -> zizmor       build/clippy/test -> build-all clippy test-ci
#   heap-check    -> heap-check      parity            -> parity-sweep
#   acceptance    -> e2e-quick       fmt               -> wfmt-check
#   playground    -> playground book-validate          docs-build -> docs-build
# Needs node (book-validate) and the wasm32 target (playground/docs-build).
ci: build-all clippy test-ci heap-check parity-sweep wfmt-check zizmor playground book-validate docs-build e2e-quick
    @echo "✓ local CI gate passed (mirrors ci.yml minus the Pages deploy)"

# Remove build artifacts.
clean:
    cargo clean
