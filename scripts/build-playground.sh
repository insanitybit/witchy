#!/usr/bin/env bash
# Build the in-browser playground: compile the witchy interpreter to
# wasm32-unknown-unknown (the library alone — no wasmtime/PM/LSP) and drop the
# module next to the static page in web/. Then serve web/ over HTTP and open it.
#
#   ./scripts/build-playground.sh
#   python3 -m http.server -d web 8000   # then visit http://localhost:8000
set -euo pipefail
cd "$(dirname "$0")/.."

# Prefer the rustup toolchain: a Homebrew rustc can't supply the wasm std
# ("can't find crate for `core`"). `rustup run stable cargo` is not enough on its
# own — if a Homebrew cargo/rustc is first on PATH, cargo still reaches for it and
# fails. Force the toolchain's own bin dir to the front of PATH and clear any
# RUSTC/RUSTFLAGS override so the wasm-capable rustc is the one that runs.
if command -v rustup >/dev/null; then
    rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
    TC_BIN="$(dirname "$(rustup which --toolchain stable rustc)")"
    RUN=(env -u RUSTC -u RUSTFLAGS "PATH=$TC_BIN:$PATH" cargo)
else
    RUN=(cargo)
fi

echo "building the witchy interpreter for wasm32-unknown-unknown..."
"${RUN[@]}" build --release --lib --no-default-features --target wasm32-unknown-unknown

WASM="target/wasm32-unknown-unknown/release/witchy.wasm"
mkdir -p web
if command -v wasm-opt >/dev/null; then
    echo "optimizing with wasm-opt..."
    wasm-opt -Oz "$WASM" -o web/witchy.wasm
else
    cp "$WASM" web/witchy.wasm
fi

SIZE=$(awk "BEGIN{printf \"%.2f\", $(wc -c < web/witchy.wasm)/1048576}")
echo "wrote web/witchy.wasm (${SIZE} MB)"
echo
echo "serve it with:   python3 -m http.server -d web 8000"
echo "then open:       http://localhost:8000"
