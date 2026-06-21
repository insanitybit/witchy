#!/usr/bin/env bash
# Build the coven-web client with the vendored, pinned toolchain (PLAN.md WS-D).
# Lint (non-blocking) -> type-check (the real gate) -> bundle to dist/app.js.
set -euo pipefail
cd "$(dirname "$0")"

BIN="tools/node_modules/.bin"

echo "[1/3] oxlint (non-blocking)"
"$BIN/oxlint" -c oxlintrc.json src/ || true

echo "[2/3] tsc --noEmit (type-check, the gate)"
"$BIN/tsc" --noEmit -p tsconfig.json

echo "[3/3] esbuild bundle -> dist/app.js"
"$BIN/esbuild" src/main.ts \
  --bundle --format=iife --target=es2022 \
  --outfile=dist/app.js --sourcemap=linked

echo "ok: dist/app.js"
