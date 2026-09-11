#!/usr/bin/env bash
# RFC-0165 A1: compile and execute both native SafeWIR security suites.
#
# Keep these package-local feature selections explicit. A workspace build can
# enable `witchy-wir/native` through the root package's dependency graph while
# a direct witchy-wir test target still has its native #[cfg] tests compiled
# out. The package-local commands make that failure impossible to hide.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo test -p witchy-wir --features native --tests
# The lowerer also contains legacy codegen fixtures in the shared deferred
# list. Scope that same policy to this package, not to a handwritten subset.
# Cargo still builds every test; canonical security tests cannot be deferred.
filter="$(./scripts/rfc0165-deferred-filter.sh scripts/rfc0165-deferred-tests.txt witchy-lower)"
cargo nextest run -p witchy-lower --features witchy-wir/native --tests -E "$filter"
