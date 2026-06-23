#!/usr/bin/env bash
# Build the coven-web client with the vendored, pinned toolchain (PLAN.md WS-D).
# Lint (non-blocking) -> type-check (the real gate) -> bundle to dist/app.js,
# bundle the in-sandbox renderer -> dist/source-sandbox.js, and compile the
# glamour highlighter rune -> dist/highlighter.wasm (WS-I/M6).
set -euo pipefail
cd "$(dirname "$0")"

BIN="tools/node_modules/.bin"
# web/ -> coven-web/ -> projects/ -> repo root.
REPO="$(cd ../../.. && pwd)"
WITCHY="$REPO/target/debug/witchy"

echo "[1/5] oxlint (non-blocking)"
"$BIN/oxlint" -c oxlintrc.json src/ || true

echo "[2/5] tsc --noEmit (type-check, the gate)"
"$BIN/tsc" --noEmit -p tsconfig.json

echo "[3/5] esbuild bundle -> dist/app.js"
"$BIN/esbuild" src/main.ts \
  --bundle --format=iife --target=es2022 \
  --outfile=dist/app.js --sourcemap=linked

# Compile the glamour highlighter rune to footprint-empty WASM. The compiler must
# see `glamour.witchy` as a resolvable sibling of the rune, so stage both in a
# scratch dir (exactly as web/witchy-runtime/demo/build.sh does).
echo "[4/5] witchy compile -> highlighter.wasm (glamour highlighter rune)"
if [[ ! -x "$WITCHY" ]]; then
  echo "build.sh: debug witchy binary not found at $WITCHY (run: cargo build)" >&2
  exit 1
fi
WORK="$(mktemp -d "${TMPDIR:-/tmp}/coven-web-highlighter.XXXXXX")"
cp "$REPO/projects/glamour/src/glamour.witchy" "$WORK/glamour.witchy"
cp "$REPO/projects/glamour/examples/highlighter/src/highlighter.witchy" "$WORK/highlighter.witchy"
( cd "$WORK" && "$WITCHY" compile highlighter.witchy --out highlighter.wasm )

# The in-sandbox renderer: bundle the entry (which imports the witchy-runtime
# pure-compute shim) into a single self-contained IIFE, then base64-INLINE the
# highlighter WASM into it (replacing the `__HIGHLIGHTER_WASM_B64__` placeholder).
# witchy's Dir `read` is UTF-8-only and cannot serve binary, and the frame is
# `connect-src 'none'` so it cannot fetch — inlining puts the bytes where the
# parent already injects the renderer as TEXT, introducing no new fetch/route.
# This bundle runs ONLY inside the opaque-origin sandbox frame, never the parent.
echo "[5/5] esbuild bundle + inline wasm -> dist/source-sandbox.js"
"$BIN/esbuild" sandbox-src/source-sandbox.js \
  --bundle --format=iife --target=es2022 \
  --outfile=dist/source-sandbox.js
WASM_PATH="$WORK/highlighter.wasm" node -e '
  const fs = require("fs");
  const out = "dist/source-sandbox.js";
  const b64 = fs.readFileSync(process.env.WASM_PATH).toString("base64");
  let js = fs.readFileSync(out, "utf8");
  if (!js.includes("__HIGHLIGHTER_WASM_B64__")) {
    throw new Error("build.sh: placeholder __HIGHLIGHTER_WASM_B64__ not found in bundle");
  }
  js = js.replace("__HIGHLIGHTER_WASM_B64__", b64);
  fs.writeFileSync(out, js);
  console.log("inlined highlighter.wasm (" + b64.length + " b64 chars) into " + out);
'
rm -rf "$WORK"

echo "ok: dist/app.js, dist/source-sandbox.js (wasm inlined)"
