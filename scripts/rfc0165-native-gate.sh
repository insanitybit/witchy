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
cargo test -p witchy-lower --features witchy-wir/native --tests
