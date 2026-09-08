# shellcheck shell=bash
ROOT=$(git rev-parse --show-toplevel)
HERE="$ROOT/scripts/rfc0165-dev"
STATUS="$HERE/status"
mkdir -p "$STATUS"
export CARGO_BUILD_RUSTC_WRAPPER=""
export CARGO_TARGET_DIR="${WITCHY_DEV_TARGET:-/private/tmp/witchy-rfc0165-inner}"
WITCHY_BIN="$CARGO_TARGET_DIR/debug/witchy"
cd "$ROOT" || exit 1
