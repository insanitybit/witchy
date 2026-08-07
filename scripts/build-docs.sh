#!/usr/bin/env bash
# Build the deployable witchy-book bundle: native static HTML, compiler-lowered
# interactive regions, and the progressively enhanced runnable-code host.
#
#   ./scripts/build-docs.sh [OUTDIR]        (default: dist; builds the browser compiler)
#   ./scripts/build-docs.sh --allow-missing-compiler [OUTDIR]  (non-runnable bundle)
#   python3 -m http.server -d dist 8000     # then open http://localhost:8000
set -euo pipefail
cd "$(dirname "$0")/.."
ALLOW_MISSING_COMPILER=0
if [[ "${1:-}" == "--allow-missing-compiler" ]]; then
  ALLOW_MISSING_COMPILER=1
  shift
fi
if [[ $# -gt 1 ]]; then
  echo "usage: build-docs.sh [--allow-missing-compiler] [OUTDIR]" >&2
  exit 2
fi
OUT="${1:-dist}"
BROWSER_COMPILER="${WITCHY_BROWSER_WASM:-}"
GENERATED_BROWSER_DIR=""
cleanup_generated_browser() {
  if [[ -n "$GENERATED_BROWSER_DIR" ]]; then rm -rf "$GENERATED_BROWSER_DIR"; fi
}
trap cleanup_generated_browser EXIT

# A complete bundle must use a compiler built from this checkout. In particular,
# never silently reuse web/witchy.wasm: it is gitignored and may predate the
# source/book by weeks while still looking like a valid artifact. CI/release
# callers that already built the exact compiler may provide WITCHY_BROWSER_WASM.
if [[ -z "$BROWSER_COMPILER" && "$ALLOW_MISSING_COMPILER" -ne 1 ]]; then
  GENERATED_BROWSER_DIR="$(mktemp -d)"
  BROWSER_COMPILER="$GENERATED_BROWSER_DIR/witchy.wasm"
  WITCHY_PLAYGROUND_OUT="$BROWSER_COMPILER" ./scripts/build-playground.sh
fi
if [[ -n "$BROWSER_COMPILER" && ! -f "$BROWSER_COMPILER" && "$ALLOW_MISSING_COMPILER" -ne 1 ]]; then
  echo "build-docs: explicit browser compiler $BROWSER_COMPILER is missing; omit WITCHY_BROWSER_WASM to build it, or pass --allow-missing-compiler for a non-runnable bundle" >&2
  exit 1
fi
BIN="${WITCHY:-}"
if [ -z "$BIN" ]; then
  # Honor a custom CARGO_TARGET_DIR (concurrent agents build into per-agent dirs).
  td="${CARGO_TARGET_DIR:-target}"
  if [ -x "$td/release/witchy" ]; then BIN="$td/release/witchy"; else BIN="$td/debug/witchy"; fi
fi

# Build all 56 routes, the counter island, manifests, runtime graph, CSS, SBOM,
# headers, and reports through the production static publisher.
"$BIN" build --web --out "$OUT" projects/docs

# 4. The browser compiler (built by build-playground.sh) — required for the Run buttons.
if [[ -n "$BROWSER_COMPILER" && -f "$BROWSER_COMPILER" ]]; then
  # Runnable Witchy fences are host facilities rather than application
  # islands. The packager records this browser boundary in the production
  # graph and injects its loader only into routes with a checked host marker.
  cp web/witchy-host.js web/witchy-runnable.js web/witchy-cell-sandbox.js \
     web/witchy-cell-frame.js web/witchy-highlight.js web/docs-static-boot.js \
     web/docs-run-options.js web/docs-asset-url.js web/wasm-fetch.js "$OUT/"
  mkdir -p "$OUT/witchy-runtime"
  cp web/witchy-runtime/witchy-runtime.mjs "$OUT/witchy-runtime/"
  cp book/examples.json "$OUT/"
  mkdir -p "$OUT/fixture-showcase"
  cp projects/fixture-showcase/src/fixture_showcase.witchy \
     projects/fixture-showcase/release.fixture.json \
     "$OUT/fixture-showcase/"
  cp "$BROWSER_COMPILER" "$OUT/witchy.wasm"
  node scripts/package-book.mjs "$OUT"
else
  echo "warning: browser compiler explicitly omitted; runnable fences remain inert" >&2
fi

echo "built $OUT/ — serve with:  python3 -m http.server -d $OUT 8000"
