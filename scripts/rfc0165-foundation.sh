#!/usr/bin/env bash
# The RFC-0165 foundation suites: the canonical SafeWIR checker, encoder,
# oracle, authority, and rule manifest (witchy-wir, native), the canonical
# lowering (witchy-lower), the admitted-launch tests, the digest-bound corpus,
# and the oracle tests. These are the security suites a landing of the
# canonical route must keep green; the blocking gate runs them as part of the
# workspace, this script runs exactly them.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_BUILD_RUSTC_WRAPPER="${CARGO_BUILD_RUSTC_WRAPPER-}"
cargo nextest run -p witchy-wir --lib --features native --no-fail-fast
cargo nextest run -p witchy-lower --lib -E 'test(/canonical/)' --no-fail-fast
cargo nextest run --test misc --features witchy-wir/native -E 'test(/^rfc0165_/)' --no-fail-fast
