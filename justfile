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
    cargo build --all-targets

# ~870 unit + integration tests. Must stay green.
test *ARGS:
    cargo test {{ARGS}}

# Lint gate — CI runs this with -D warnings.
clippy:
    cargo clippy --all-targets -- -D warnings

alias lint := clippy

# NOTE: there is deliberately NO `cargo fmt` recipe. The Rust in this repo is
# hand-formatted on purpose (see scripts/check.sh); `cargo fmt` reformats ~71
# files and fights the intended style. The only formatting gate is `witchy fmt`
# over std/ + examples/ — see `wfmt` / `wfmt-check` below.

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
parity-sweep: build-release
    #!/usr/bin/env bash
    set -uo pipefail
    fail=0
    for f in examples/*/src/*.witchy; do
        out=$({{bin}} parity "$f" 2>&1) || true
        if echo "$out" | grep -qi "DIVERGE"; then
            echo "DIVERGENCE: $f"
            echo "$out"
            fail=1
        fi
    done
    exit $fail

# Format every std + example witchy file in place.
wfmt: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    for f in std/*.witchy examples/*/src/*.witchy; do
        {{bin}} fmt "$f"
    done

# Verify std + example formatting without writing (CI fmt job).
wfmt-check: build-release
    #!/usr/bin/env bash
    set -uo pipefail
    fail=0
    for f in std/*.witchy examples/*/src/*.witchy; do
        if ! {{bin}} fmt --check "$f" >/dev/null 2>&1; then
            echo "needs formatting: $f"
            fail=1
        fi
    done
    exit $fail

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

# Build "The witchy Book" into ./book-html.
book:
    ./scripts/build-book.sh

# Build the book and serve it with live reload.
book-serve:
    ./scripts/build-book.sh --serve

# Build the in-browser playground (wasm) into web/.
playground:
    ./scripts/build-playground.sh

# Build just the wasm interpreter the way CI does (no native deps).
wasm:
    cargo build --release --lib --no-default-features --target wasm32-unknown-unknown

# --- Aggregates -----------------------------------------------------------

# The full local gate — everything CI runs. Run before opening a PR.
ci: build-all clippy test parity-sweep wfmt-check e2e-quick
    @echo "✓ local CI gate passed"

# Remove build artifacts.
clean:
    cargo clean
