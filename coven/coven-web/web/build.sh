#!/usr/bin/env bash
# Build the coven-web client with the vendored, pinned toolchain (PLAN.md WS-D).
# Lint (non-blocking) -> type-check (the real gate) -> bundle the bootstrap host
# shell to dist/app.js + INLINE the glamour coven-web app WASM into it (RFC-0015
# Phase D), bundle the in-sandbox renderer -> dist/source-sandbox.js, and compile
# the glamour highlighter rune -> dist/highlighter.wasm (WS-I/M6).
#
# The ENTIRE frontend is now one empty-root-footprint glamour rune
# (projects/glamour/examples/coven_web_app) compiled to WASM; src/ is just the thin
# host shell (the bootstrap + the session/WebAuthn/yank ports) that holds authority.
set -euo pipefail
cd "$(dirname "$0")"

BIN="tools/node_modules/.bin"
# web/ -> coven-web/ -> projects/ -> repo root.
REPO="$(cd ../../.. && pwd)"

# BUG-029: accept whichever witchy is available rather than hardcoding the debug build.
# Precedence: $WITCHY override, then the release build, then debug, then PATH.
WITCHY="${WITCHY:-}"
if [[ -z "$WITCHY" ]]; then
  if [[ -x "$REPO/target/release/witchy" ]]; then WITCHY="$REPO/target/release/witchy"
  elif [[ -x "$REPO/target/debug/witchy" ]]; then WITCHY="$REPO/target/debug/witchy"
  elif command -v witchy >/dev/null 2>&1; then WITCHY="$(command -v witchy)"
  fi
fi
if [[ -z "$WITCHY" || ! -x "$WITCHY" ]]; then
  echo "build.sh: no witchy binary found. Set \$WITCHY, run 'cargo build [--release]', or put witchy on PATH." >&2
  exit 1
fi

# BUG-164: the vendored build tools live under tools/node_modules, which is gitignored and may be
# absent on a fresh checkout. Under 'set -e' a missing tool would fail cryptically, so bootstrap
# it (npm ci from the pinned tools/package-lock.json) or fail with a clear, actionable message.
if [[ ! -x "$BIN/esbuild" || ! -x "$BIN/tsc" || ! -x "$BIN/oxlint" ]]; then
  if command -v npm >/dev/null 2>&1; then
    echo "[0/6] bootstrapping vendored build tools (tools/ via npm ci)"
    ( cd tools && npm ci --no-audit --no-fund )
  else
    echo "build.sh: build tools missing (tools/node_modules) and 'npm' not found on PATH." >&2
    echo "         Install Node/npm and re-run, or run 'npm ci' in $(pwd)/tools yourself." >&2
    exit 1
  fi
fi

echo "[1/6] oxlint (non-blocking)"
"$BIN/oxlint" -c oxlintrc.json src/ || true

echo "[2/6] tsc --noEmit (type-check, the gate)"
"$BIN/tsc" --noEmit -p tsconfig.json

# The bootstrap imports the glamour DOM host shell under the bare specifier `glamour-dom`
# (typed via src/glamour-dom.d.ts); map it to the real shell module here. No sourcemap:
# the wasm-inline step below rewrites app.js, which would desync a linked map.
echo "[3/6] esbuild bundle host shell -> dist/app.js"
"$BIN/esbuild" src/main.ts \
  --bundle --format=iife --target=es2022 \
  --alias:glamour-dom="$REPO/web/witchy-runtime/glamour-dom.mjs" \
  --outfile=dist/app.js

# BUG-046: emit the stylesheet link AND the file it points at. The server registers a
# /styles.css route (coven_web.witchy) under `style-src 'self'`, but index.html never linked
# it and dist/styles.css was never written, so the app rendered unstyled and /styles.css 404'd.
# The stylesheet source is checked in at src/styles.css (unlike gitignored dist/), so copy it.
cat > dist/index.html <<'HTML'
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Coven Web</title>
  <link rel="stylesheet" href="/styles.css">
</head>
<body>
  <main id="app" aria-label="Coven registry browser"></main>
  <script src="/app.js"></script>
</body>
</html>
HTML

cp src/styles.css dist/styles.css

# Compile the glamour coven-web APP rune to footprint-empty WASM and base64-INLINE it into
# the bootstrap bundle (replacing `__APP_WASM_B64__`). Same rationale as the highlighter
# below: witchy's Dir `read` is UTF-8-only so a `.wasm` route is impossible, and inlining
# keeps the module riding in the bundle the server already serves as TEXT (`script-src
# 'self'`); the parent compiles it under the app CSP's `'wasm-unsafe-eval'`. The compiler
# must see `glamour.witchy` + `markdown.witchy` as resolvable siblings, so stage them.
echo "[4/6] witchy compile coven_web_app -> inline into dist/app.js"
WORK_APP="$(mktemp -d "${TMPDIR:-/tmp}/coven-web-app.XXXXXX")"
cp "$REPO/projects/glamour/src/glamour.witchy" "$WORK_APP/glamour.witchy"
cp "$REPO/projects/glamour/src/markdown.witchy" "$WORK_APP/markdown.witchy"
cp "$REPO/projects/glamour/src/highlight.witchy" "$WORK_APP/highlight.witchy"
cp "$REPO/projects/glamour/examples/coven_web_app/src/coven_web_app.witchy" "$WORK_APP/coven_web_app.witchy"
( cd "$WORK_APP" && "$WITCHY" compile coven_web_app.witchy --out coven_web_app.wasm )
APP_WASM_PATH="$WORK_APP/coven_web_app.wasm" node -e '
  const fs = require("fs");
  const out = "dist/app.js";
  const b64 = fs.readFileSync(process.env.APP_WASM_PATH).toString("base64");
  let js = fs.readFileSync(out, "utf8");
  if (!js.includes("__APP_WASM_B64__")) {
    throw new Error("build.sh: placeholder __APP_WASM_B64__ not found in bundle");
  }
  js = js.replace("__APP_WASM_B64__", b64);
  fs.writeFileSync(out, js);
  console.log("inlined coven_web_app.wasm (" + b64.length + " b64 chars) into " + out);
'
rm -rf "$WORK_APP"

# Compile the glamour highlighter rune to footprint-empty WASM. The compiler must
# see `glamour.witchy` as a resolvable sibling of the rune, so stage both in a
# scratch dir (exactly as web/witchy-runtime/demo/build.sh does).
echo "[5/6] witchy compile -> highlighter.wasm (glamour highlighter rune)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/coven-web-highlighter.XXXXXX")"
cp "$REPO/projects/glamour/src/glamour.witchy" "$WORK/glamour.witchy"
cp "$REPO/projects/glamour/src/highlight.witchy" "$WORK/highlight.witchy"
cp "$REPO/projects/glamour/examples/highlighter/src/highlighter.witchy" "$WORK/highlighter.witchy"
( cd "$WORK" && "$WITCHY" compile highlighter.witchy --out highlighter.wasm )

# The in-sandbox renderer: bundle the entry (which imports the witchy-runtime
# deny-all capability shim) into a single self-contained IIFE, then base64-INLINE the
# highlighter WASM into it (replacing the `__HIGHLIGHTER_WASM_B64__` placeholder).
# witchy's Dir `read` is UTF-8-only and cannot serve binary, and the frame is
# `connect-src 'none'` so it cannot fetch — inlining puts the bytes where the
# parent already injects the renderer as TEXT, introducing no new fetch/route.
# This bundle runs ONLY inside the opaque-origin sandbox frame, never the parent.
echo "[6/6] esbuild bundle + inline wasm -> dist/source-sandbox.js"
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

echo "ok: dist/app.js (glamour app wasm inlined), dist/source-sandbox.js (highlighter wasm inlined)"
