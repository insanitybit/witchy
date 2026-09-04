# BUG: canonical WasmIR execution tests required `native` without a feature gate

## Impact

`witchy-wir` library tests without the `native` feature failed to compile after
canonical WasmIR execution coverage was added. The tests referenced Wasmtime,
which is an optional dependency available only under `native`. This could hide
whether the non-native crate profile and its route-gating test remained usable.

## Cause

Seven Wasmtime execution tests in `canonical_wasmir.rs` lacked
`#[cfg(feature = "native")]`, unlike the dependency they exercised.

## Fix and evidence

The execution tests are now gated by `native`; validation and structural tests
remain available without it. The following focused commands pass:

```sh
CARGO_TARGET_DIR=target-codex-rfc0165 cargo test -p witchy-wir --features native --lib canonical_wasmir
CARGO_TARGET_DIR=target-codex-rfc0165 cargo test -p witchy-wir --lib tail
CARGO_TARGET_DIR=target-codex-rfc0165 cargo test -p witchy-wir --test rfc0165_route_gate
```

This finding concerns test availability, not a demonstrated production Wasm
memory-safety bypass.
