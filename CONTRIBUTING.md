# Contributing to witchy

## Build and test

```sh
cargo build                             # debug build of the `witchy` CLI
cargo nextest run --workspace           # the unit + integration suite (must stay green)
cargo clippy --workspace -- -D warnings # lint gate (CI enforces)
./scripts/e2e-full.sh       # the from-scratch acceptance test (see below)
```

The end-to-end package-manager tests (`tests/e2e.rs`) drive the real binary
through scaffold/publish/add/build/run against hermetic per-test registries.

`scripts/e2e-full.sh` is the whole-system acceptance run: it builds from
scratch, then asserts one program produces identical output on all three
backends, exercises the formatter, the in-language test framework, capability
auditing (`caps`/`caps-diff`), sandbox enforcement (confinement + allowlist
refusals), a complete registry lifecycle (trusted publish → staged → 2FA
promote → verified add → the capability-widening gate → namespace binding), a
multi-rune example project, and doc extraction — ~30 asserted checks.
`--quick` skips the test stage (CI runs it as its own job).

## The one rule: parity

witchy has two backends (interpreter = reference, compiled WASM) held to
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
in `src/example_tests.rs`). If a backend genuinely can't support it
yet, make it a **loud error** there — never a silently different answer.
Behavior that errors should error on *both* backends (the parity tool checks
error paths too).

## Formatting

Rust code: `cargo fmt`. witchy code (std/, examples/): `witchy fmt <file>` —
CI runs `witchy fmt --check` over the tree. If you edit `std/`, regenerate the
API reference: `witchy doc std/*.witchy > spec/stdlib.md` (a test asserts it
is current).

## Documentation is tested

Every ` ```witchy ` fenced block in the markdown docs (`README.md`, `docs/*.md`,
…) is verified by the `documentation_examples_are_valid` test: it must parse,
link, and type-check, and a `Console`-only `main` is run on both backends with
the outputs compared. So examples in the docs must be **complete, correct
programs** — when you change the language, the docs that demonstrate it fail the
build until updated. Genuinely partial snippets (signatures with `...`, shell
commands, sample output) use a different fence (untagged, or ` ```sh `) and are
not executed.

## Where things live

See [spec/architecture.md](spec/architecture.md) for the pipeline and the
workspace layout (the compiler is split into stage-aligned crates under
`crates/`). Quick orientation: the interpreter
(`crates/witchy-interp/src/interpreter.rs`) defines semantics; codegen
(`crates/witchy-lower/src/codegen.rs`) must match it; typeck
(`crates/witchy-types/src/typeck.rs`) rejects what can't be made to agree; the
wasmtime sandbox (`crates/witchy-runtime/src/runtime.rs`) is the security
boundary (capability-gated host imports — anything you add there is part of the
TCB, so keep host functions small, total, and confined).

## Capability changes

If a change adds or widens what any capability can do, update the footprint
analyzer (`crates/witchy-caps/src/capabilities.rs`), the runtime gating
(`crates/witchy-runtime/src/runtime.rs`), and
[spec/capabilities.md](spec/capabilities.md) together — and add an
*enforcement* test (an ungranted module must fail to instantiate).

## License

Dual MIT / Apache-2.0. By contributing you agree your work is licensed the
same way.
