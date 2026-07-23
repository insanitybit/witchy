#!/usr/bin/env bash
# Build the deployable witchy-book bundle (RFC-0041): the glamour docs app compiled to wasm,
# the book content, the browser compiler + shared web modules, and the classification manifest —
# a set of static files servable anywhere (GitHub Pages, `python3 -m http.server`), NO server.
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

rm -rf "$OUT"
mkdir -p "$OUT/content"

# 1. Compile the docs app (glamour + markdown as siblings) to wasm.
tmp="$(mktemp -d)"
cp projects/glamour/src/glamour.witchy projects/glamour/src/markdown.witchy \
   projects/docs/src/docs.witchy "$tmp/"
"$BIN" compile "$tmp/docs.witchy" --out "$OUT/docs.wasm"
rm -rf "$tmp"

# 1b. Compile the INTERACTIVE demo app(s) mounted live in the book (RFC-0041) — each a small
# glamour app the docs page mounts with the same runtime, network denied. `glamour` resolves
# from the bundled-rune path (no adjacent copy needed).
"$BIN" compile projects/docs/src/counter.witchy --out "$OUT/counter.wasm"

# 2. Stage the book content (SUMMARY + every page) under /content/, where the app fetches it.
cp book/src/*.md "$OUT/content/"

# 3. The shared web modules (flat — they import each other as siblings), the page, the manifest.
cp web/witchy-runtime/glamour-dom.mjs web/witchy-runtime/witchy-runtime.mjs \
   web/witchy-host.js web/witchy-runnable.js web/witchy-highlight.js \
   web/docs-boot.js web/docs-run-options.js web/docs-asset-url.js \
   web/wasm-fetch.js web/docs.css "$OUT/"
cp web/docs.html "$OUT/index.html"
cp book/examples.json "$OUT/"
# Strict cross-origin isolation on every response (house rule) — for a `_headers`-honoring host.
cp web/_headers "$OUT/"

# 4. The browser compiler (built by build-playground.sh) — required for the Run buttons.
if [[ -n "$BROWSER_COMPILER" && -f "$BROWSER_COMPILER" ]]; then
  cp "$BROWSER_COMPILER" "$OUT/witchy.wasm"
else
  echo "warning: browser compiler explicitly omitted; Run buttons will not work" >&2
fi

echo "built $OUT/ — serve with:  python3 -m http.server -d $OUT 8000"
