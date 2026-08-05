#!/usr/bin/env bash
# Launch the self-hosted (witchy) coven registry — the real, dogfooded registry
# written in witchy itself (projects/coven). One command: generates a signing key
# if absent, picks sensible defaults, and starts the server.
#
#   scripts/run-witchy-coven.sh [addr] [store-dir] [issuer=pubkeyhex ...]
#
# Defaults: addr=127.0.0.1:8787, store=./coven-store. Trailing `issuer=pubkeyhex`
# args register trusted issuers (trusted publishing); with none the registry is
# anonymous (local mode).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COVEN_SRC="$REPO/projects/coven/src/coven.witchy"

ADDR="${1:-127.0.0.1:8787}"; [ $# -gt 0 ] && shift
STORE="${1:-$PWD/coven-store}"; [ $# -gt 0 ] && shift
# whatever remains in "$@" are issuer specs, passed through to coven

# Locate the witchy binary — pick whichever of release/debug is NEWER (so a stale
# release build can't shadow a fresh debug one); build debug if neither exists.
DEBUG="$REPO/target/debug/witchy"
RELEASE="$REPO/target/release/witchy"
if [ -x "$RELEASE" ] && { [ ! -x "$DEBUG" ] || [ "$RELEASE" -nt "$DEBUG" ]; }; then
    BIN="$RELEASE"
elif [ -x "$DEBUG" ]; then
    BIN="$DEBUG"
else
    echo "building witchy…" >&2
    (cd "$REPO" && cargo build --quiet)
    BIN="$DEBUG"
fi

mkdir -p "$STORE"
SEED="$STORE/root.seed"
if [ ! -f "$SEED" ]; then
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex 32 > "$SEED"
    else
        head -c 32 /dev/urandom | xxd -p -c 32 > "$SEED"
    fi
    echo "generated a new registry signing key at $SEED" >&2
fi

echo "starting witchy coven at http://$ADDR  (store: $STORE)" >&2
# coven's Dir capability is its working directory; run from the store.
cd "$STORE"
exec "$BIN" --net "$ADDR" --signing-key "$SEED" "$COVEN_SRC" "$ADDR" "$@"
