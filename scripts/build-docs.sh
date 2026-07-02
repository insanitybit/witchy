#!/usr/bin/env bash
# Build the deployable witchy-book bundle (RFC-0041): the glamour docs app compiled to wasm,
# the book content, the browser compiler + shared web modules, and the classification manifest —
# a bag of static files servable anywhere (GitHub Pages, `python3 -m http.server`), NO server.
#
#   ./scripts/build-docs.sh [OUTDIR]        (default: dist)
#   python3 -m http.server -d dist 8000     # then open http://localhost:8000
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-dist}"
BIN="${WITCHY:-}"
if [ -z "$BIN" ]; then
  if [ -x ./target/release/witchy ]; then BIN=./target/release/witchy; else BIN=./target/debug/witchy; fi
fi

rm -rf "$OUT"
mkdir -p "$OUT/content"

# 1. Compile the docs app (glamour + markdown as siblings) to wasm.
tmp="$(mktemp -d)"
cp projects/glamour/src/glamour.witchy projects/glamour/src/markdown.witchy \
   projects/docs/src/docs.witchy "$tmp/"
"$BIN" compile "$tmp/docs.witchy" --out "$OUT/docs.wasm"
rm -rf "$tmp"

# 2. Stage the book content (SUMMARY + every page) under /content/, where the app fetches it.
cp book/src/*.md "$OUT/content/"

# 3. The shared web modules (flat — they import each other as siblings), the page, the manifest.
cp web/witchy-runtime/glamour-dom.mjs web/witchy-runtime/witchy-runtime.mjs \
   web/witchy-host.js web/witchy-runnable.js web/witchy-highlight.js \
   web/docs-boot.js web/docs.css "$OUT/"
cp web/docs.html "$OUT/index.html"
cp book/examples.json "$OUT/"
# Strict cross-origin isolation on every response (house rule) — for a `_headers`-honoring host.
cp web/_headers "$OUT/"

# 4. The browser compiler (built by build-playground.sh) — required for the Run buttons.
if [ -f web/witchy.wasm ]; then
  cp web/witchy.wasm "$OUT/"
else
  echo "warning: web/witchy.wasm missing — Run buttons won't work until you run ./scripts/build-playground.sh" >&2
fi

echo "built $OUT/ — serve with:  python3 -m http.server -d $OUT 8000"
