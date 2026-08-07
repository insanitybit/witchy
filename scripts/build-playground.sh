#!/usr/bin/env bash
# Build the in-browser playground: compile the witchy interpreter to
# wasm32-unknown-unknown (the library alone — no wasmtime/PM/LSP) and drop the
# module next to the static page in web/. Then serve web/ over HTTP and open it.
#
#   ./scripts/build-playground.sh
#   python3 -m http.server -d web 8000   # then visit http://localhost:8000
set -euo pipefail
cd "$(dirname "$0")/.."

# The standalone playground writes beside the static page. Bundle builders can
# request a private output path so they never trust or overwrite an older
# gitignored web/witchy.wasm from another checkout/build.
OUT="${WITCHY_PLAYGROUND_OUT:-web/witchy.wasm}"

# Prefer the rustup toolchain: a Homebrew rustc can't supply the wasm std
# ("can't find crate for `core`"). `rustup run stable cargo` is not enough on its
# own — if a Homebrew cargo/rustc is first on PATH, cargo still reaches for it and
# fails. Force the toolchain's own bin dir to the front of PATH and clear any
# RUSTC/RUSTFLAGS override so the wasm-capable rustc is the one that runs.
# Resolve the ACTIVE toolchain, not the literal `stable`: CI pins a versioned
# toolchain (see .github/actions/install-rust) and adds the wasm target to it,
# while the runner's preinstalled `stable` has no wasm std at all.
if [ -n "${CARGO:-}" ]; then
    RUN=(env RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER= "$CARGO")
elif command -v rustup >/dev/null; then
    rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
    TC_BIN="$(dirname "$(rustup which rustc)")"
    RUN=(env -u RUSTC -u RUSTFLAGS RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER= "PATH=$TC_BIN:$PATH" cargo)
else
    RUN=(env RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER= cargo)
fi

echo "building the witchy interpreter for wasm32-unknown-unknown..."
"${RUN[@]}" build --release --lib --no-default-features --features browser-fixtures \
    --target wasm32-unknown-unknown

# Honor a custom CARGO_TARGET_DIR — the cargo build above wrote the module there.
WASM="${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/release/witchy.wasm"
mkdir -p "$(dirname "$OUT")"
if [ "${WITCHY_SKIP_WASM_OPT:-0}" != "1" ] && command -v wasm-opt >/dev/null; then
    echo "optimizing with wasm-opt..."
    wasm-opt -Oz "$WASM" -o "$OUT"
else
    cp "$WASM" "$OUT"
fi

SIZE=$(awk "BEGIN{printf \"%.2f\", $(wc -c < "$OUT")/1048576}")
echo "wrote $OUT (${SIZE} MB)"
echo
echo "serve it with:   python3 -m http.server -d web 8000"
echo "then open:       http://localhost:8000"
