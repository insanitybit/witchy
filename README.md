# Witchy

- [`witchyc/`](witchyc/) contains the compiler, [standard library](witchyc/docs/standard-library.md), proof tooling, and target tests. Within `src/compiler/`, the [checker](witchyc/src/compiler/check/), [Wasm emitter](witchyc/src/compiler/emit/), and [scalar proof generator](witchyc/src/compiler/scalar_vc/) are organized by responsibility.
- [`witchyc/runtime/`](witchyc/runtime/) holds the native guest runtime. The compiler builds it as a separate Rust archive and links it into native executables. Cranelift lowering lives in [`src/compiler/native/`](witchyc/src/compiler/native/). The [Wasmtime host](witchyc/tools/wasmtime-host/) is a separate executable.
- [`grimoire/`](grimoire/) contains the package manager and package test runner. Grimoire builds, reviews, runs, and publishes packages.
- [`coven/`](coven/) contains the registry, its operator tools, and its web and deployment code.

Use MBX for Rust builds. Build the compiler and Wasmtime host into a common target directory to use them with the Grimoire tests.

```sh
export CARGO_TARGET_DIR="$PWD/witchyc/target"
mbx build --release --locked --manifest-path witchyc/Cargo.toml
mbx build --release --locked --manifest-path witchyc/tools/wasmtime-host/Cargo.toml
python3 grimoire/tests/run.py
```

The Grimoire test runner uses executables in this checkout when available. Set `WITCHYC`, `WITCHY_WASMTIME_HOST`, or `COVEN` to select a different executable.

From the repository root, run the additional Grimoire and Coven suites with the same build.

```sh
export PATH="$PWD/grimoire:$CARGO_TARGET_DIR/release:$PATH"
python3 grimoire/tests/run.py --process-boundaries
python3 grimoire/tests/run.py --http-witness
python3 coven/witchy2/run_tests.py --all --packages
```

See [compiler development](witchyc/docs/development.md) for the proof checker setup and compiler test commands, and [Coven's test setup](coven/witchy2/README.md) for the crypto provider and resident host used by the timed suite.

Coven's SQLite workers declare `@sqlite` in their adjacent `.packages` files. The checkout's `coven/witchy2/vendor/sqlite` points to the canonical package under `witchyc/packages/`. The installer copies the package source into the same relative location beside the installed guests.

Coven's approval and release workers use the capability-free [WebAuthn verification package](witchyc/packages/webauthn/README.md) through pinned `@webauthn` imports. Coven owns enrollment, challenge and origin checks, and the SQLite counter transaction.
