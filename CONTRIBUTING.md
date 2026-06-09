# Contributing to witchy

## Build and test

```sh
cargo build                 # debug build of the `witchy` CLI
cargo test                  # ~870 unit + integration tests (must stay green)
cargo clippy -- -D warnings # lint gate (CI enforces)
```

The end-to-end package-manager tests (`tests/e2e.rs`) drive the real binary
through scaffold/publish/add/build/run against hermetic per-test registries.

## The one rule: parity

witchy has three backends (interpreter = reference, WASM, native) held to
**zero silent divergence**. Before opening a PR:

```sh
witchy parity path/to/program.witchy        # one program, both backends
for f in examples/*.witchy; do              # the sweep CI runs
    ./target/release/witchy parity "$f"
done
```

If you add observable behavior (a builtin, an operator, a stdlib function),
implement it on the interpreter AND the WASM backend in the same change, with
a differential test (`assert_eq!(interp(src), ...); assert_eq!(run_on_wasm(src), ...)`
in `src/main.rs`'s `example_tests`). If a backend genuinely can't support it
yet, make it a **loud error** there — never a silently different answer.
Behavior that errors should error on *both* backends (the parity tool checks
error paths too).

## Formatting

Rust code: `cargo fmt`. witchy code (std/, examples/): `witchy fmt <file>` —
CI runs `witchy fmt --check` over the tree. If you edit `std/`, regenerate the
API reference: `witchy doc std/*.witchy > docs/stdlib.md` (a test asserts it
is current).

## Where things live

See [docs/architecture.md](docs/architecture.md) for the pipeline and file
map. Quick orientation: the interpreter (`src/interpreter.rs`) defines
semantics; `src/codegen.rs` must match it; `src/typeck.rs` rejects what can't
be made to agree; `src/runtime.rs` is the security boundary (capability-gated
host imports — anything you add there is part of the TCB, so keep host
functions small, total, and confined).

## Capability changes

If a change adds or widens what any capability can do, update the footprint
analyzer (`src/capabilities.rs`), the runtime gating (`src/runtime.rs`), and
[docs/capabilities.md](docs/capabilities.md) together — and add an
*enforcement* test (an ungranted module must fail to instantiate).

## License

Dual MIT / Apache-2.0. By contributing you agree your work is licensed the
same way.
