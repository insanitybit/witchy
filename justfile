# justfile — task runner for the witchy project.
# Recipes expose CI jobs, optional local tools, helper scripts, and `witchy`
# CLI subcommands. Run `just` for the list.

set shell := ["bash", "-uc"]

# The release binary used by formatting and CLI passthrough recipes.
bin := "./target/release/witchy"

# Show the available recipes (default when you run bare `just`).
default:
    @just --list

# --- Rust: build / lint / test -------------------------------------------

# Debug build of the `witchy` CLI.
build:
    cargo build

# Release build used by formatting, CLI passthroughs, and helper scripts.
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

# Optional local lint; Clippy is not part of the merge or CI gate.
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

# --- Witchy formatting (mirrors CI) --------------------------------------

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

# Acceptance run without the `cargo test` stage.
e2e-quick:
    ./scripts/e2e-full.sh --quick

# --- Book & playground ----------------------------------------------------

# Build "The witchy Book" as native static HTML plus compiler-lowered regions
# into ./dist. This form explicitly omits the browser compiler, so runnable
# fences remain inert. Use `just docs-build` (or `book-serve`) for editable cells.
book: build-release
    ./scripts/build-docs.sh --allow-missing-compiler dist

# Build the complete bundle (fresh browser compiler and audited runnable host),
# then serve it locally at http://localhost:8000.
book-serve: docs-build
    python3 -m http.server -d dist 8000

# Build the in-browser playground (wasm) into web/.
playground:
    ./scripts/build-playground.sh

# Build the deployable docs bundle into ./dist. The bundle
# builder generates its browser compiler from the same checkout.
docs-build: build-release
    ./scripts/build-docs.sh dist

# Build the wasm library without native runtime dependencies.
wasm:
    cargo build --release --lib --no-default-features --target wasm32-unknown-unknown

# --- Aggregates -----------------------------------------------------------

# The full local gate mirrors the blocking compiler jobs in ci.yml.
# See .github/workflows/ci.yml for the authoritative set.
# zizmor maps to `zizmor`, build/test to `build-all test-ci`, and fmt to
# `wfmt-check`.
ci: build-all test-ci wfmt-check zizmor
    @echo "✓ local CI gate passed (mirrors ci.yml)"

# Remove build artifacts.
clean:
    cargo clean
