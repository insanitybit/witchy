#!/usr/bin/env bash
# build.sh — compile the glamour example runes to footprint-empty WASM for the
# real-browser demo. Each rune `import glamour`, so the compiler must see
# `glamour.witchy` as a resolvable sibling; we stage each rune next to a copy of
# glamour in a scratch dir (exactly what the headless tests do), compile with the
# DEBUG witchy binary, and drop the `.wasm` into this `demo/` directory.
#
# Usage:  ./build.sh        (run from web/witchy-runtime/demo/)
# Output: demo/counter.wasm, demo/highlighter.wasm, demo/runecard.wasm,
#         demo/covenbrowser.wasm
set -euo pipefail

# Resolve paths relative to this script so it works from any CWD.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
WITCHY="$REPO/target/debug/witchy"
GLAMOUR="$REPO/projects/glamour/src/glamour.witchy"

if [[ ! -x "$WITCHY" ]]; then
  echo "build.sh: debug witchy binary not found at $WITCHY" >&2
  echo "  (build it once with: cargo build)" >&2
  exit 1
fi

# Compile one rune: $1 = name, $2 = path to the rune's .witchy source.
compile_rune() {
  local name="$1" src="$2"
  local work
  work="$(mktemp -d "${TMPDIR:-/tmp}/witchy-demo-${name}.XXXXXX")"
  # Stage glamour + the rune as siblings so `import glamour` resolves.
  cp "$GLAMOUR" "$work/glamour.witchy"
  cp "$src" "$work/$name.witchy"
  ( cd "$work" && "$WITCHY" compile "$name.witchy" --out "$name.wasm" )
  cp "$work/$name.wasm" "$HERE/$name.wasm"
  rm -rf "$work"
  echo "build.sh: wrote $HERE/$name.wasm"
}

compile_rune counter \
  "$REPO/projects/glamour/examples/counter/src/counter.witchy"
compile_rune highlighter \
  "$REPO/projects/glamour/examples/highlighter/src/highlighter.witchy"
compile_rune runecard \
  "$REPO/projects/glamour/examples/runecard/src/runecard.witchy"
compile_rune covenbrowser \
  "$REPO/projects/glamour/examples/covenbrowser/src/covenbrowser.witchy"

echo "build.sh: done. Serve with:  (cd $REPO/web/witchy-runtime && python3 -m http.server 8099)  then open /demo/"
