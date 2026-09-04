#!/usr/bin/env bash
# Focused RFC-0165 A2 route-gate test. It never invokes Cargo.
set -euo pipefail
cd "$(dirname "$0")/.."

tmp="$(mktemp -d "${TMPDIR:-/tmp}/witchy-rfc0165-route-gate.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/crates/witchy-wir/src" "$tmp/crates/witchy-lower/src/codegen"

cat >"$tmp/Cargo.toml" <<'EOF'
[package]
name = "fixture"
version = "0.1.0"
edition = "2024"

[dev-dependencies]
witchy-wir = { path = "crates/witchy-wir", features = ["raw-encoder-test-api"] }
EOF
cat >"$tmp/crates/witchy-wir/Cargo.toml" <<'EOF'
[package]
name = "witchy-wir"
version = "0.1.0"
edition = "2024"

[features]
raw-encoder-test-api = []
EOF
cat >"$tmp/crates/witchy-lower/Cargo.toml" <<'EOF'
[package]
name = "witchy-lower"
version = "0.1.0"
edition = "2024"

[dependencies]
witchy-wir = { path = "../witchy-wir" }
EOF
cat >"$tmp/crates/witchy-wir/src/lib.rs" <<'EOF'
mod canonical_wasmir;
pub mod wir_encode;
EOF
cat >"$tmp/crates/witchy-wir/src/canonical_wasmir.rs" <<'EOF'
struct ValidatedSafeWir;
struct WasmIr {
    value: usize,
}

fn lower_validated(module: &ValidatedSafeWir) -> WasmIr {
    let _ = module;
    WasmIr { value: 0 }
}

pub fn admit_canonical(module: ValidatedSafeWir) {
    let _ = lower_validated(&module);
}
EOF
cat >"$tmp/crates/witchy-wir/src/wir_encode.rs" <<'EOF'
#[cfg(any(test, feature = "raw-encoder-test-api"))]
pub fn encode(module: &WirModule) {
    let _ = module;
}
EOF
cat >"$tmp/crates/witchy-lower/src/codegen/assembly.rs" <<'EOF'
pub fn compile() {}
EOF

gate="scripts/rfc0165-route-gate.sh"
run_gate() {
    bash "$gate" --root "$tmp"
}
run_gate >/dev/null

reject() {
    local label="$1"
    if run_gate >"$tmp/$label.out" 2>"$tmp/$label.err"; then
        printf '%s\n' "test-rfc0165-route-gate: $label unexpectedly accepted" >&2
        exit 1
    fi
}

printf '%s\n' 'struct WasmIr;' >"$tmp/crates/witchy-wir/src/rogue.rs"
reject outside-wasmir
rm "$tmp/crates/witchy-wir/src/rogue.rs"

sed -i '' 's/&ValidatedSafeWir/\&OtherType/' "$tmp/crates/witchy-wir/src/canonical_wasmir.rs"
reject unbound-wasmir
sed -i '' 's/\&OtherType/\&ValidatedSafeWir/' "$tmp/crates/witchy-wir/src/canonical_wasmir.rs"

sed -i '' 's/raw-encoder-test-api/legacy-encoder-bridge/' "$tmp/crates/witchy-wir/src/wir_encode.rs"
reject legacy-encoder-surface
sed -i '' 's/legacy-encoder-bridge/raw-encoder-test-api/' "$tmp/crates/witchy-wir/src/wir_encode.rs"

sed -i '' 's/path = "..\/witchy-wir"/path = "..\/witchy-wir", features = ["raw-encoder-test-api"]/' \
    "$tmp/crates/witchy-lower/Cargo.toml"
reject raw-production-feature
sed -i '' 's/, features = \["raw-encoder-test-api"\]//' "$tmp/crates/witchy-lower/Cargo.toml"

printf '%s\n' 'pub fn compile_raw(module: &WirModule) { wir_encode::encode(module); }' \
    >>"$tmp/crates/witchy-lower/src/codegen/assembly.rs"
reject lowerer-raw-call

printf '%s\n' "test-rfc0165-route-gate: ok (private WasmIr, test-only encoder, feature tree, caller boundary)"
