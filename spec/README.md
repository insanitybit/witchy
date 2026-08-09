# spec/

The authoritative, always-current description of what witchy **is** today. If
`spec/` and the code disagree, one of them is a bug - they are meant to match.

## Rules

- **Describe what IS, never history.** No "we used to…", no "this replaces…", no
  diffs against past decisions. That belongs in [`rfcs/`](../rfcs/) and in commit
  messages. A reader of `spec/` should learn the current language, not its past.
- **No proposals.** Anything that isn't shipped-and-current goes to `rfcs/`.
- **Keep prose thin; anchor to runnable truth.** Prose drifts; tests don't.
  Where a spec section has examples, prefer examples that are actually executed
  by the test suite (e.g. `src/example_tests.rs`) so CI fails when the spec lies.
  Untestable prose is the danger zone - minimize it.
- **Stamp freshness.** A spec doc may carry `verified: <commit>` frontmatter
  recording the last commit its claims were checked against. Run
  `./scripts/check-spec-freshness.sh` to validate those commits and report their
  age, or add `--strict` to fail when a stamp is more than 250 commits behind
  `HEAD`. Age is advisory by default because a stamp doesn't declare which
  source files can invalidate its claims; runnable examples and generated
  references remain the hard freshness gates.

## What lives here

The current language reference, the stdlib reference, the capability model, the
runtime/architecture as it actually exists - the things a user or contributor
needs to know to use witchy *right now*:

- [`language.md`](language.md) - syntax and semantics reference (the single best
  starting point; every `witchy` block is an executed program).
- [`capabilities.md`](capabilities.md) - the security model: authority-as-a-value,
  rights, narrowing, the `File`/`Dir`/`Net`/`Exec`/`Secret` capabilities, grant
  documents, and the sandbox.
- [`stdlib.md`](stdlib.md) - the module-by-module API. **Generated** from
  `std/*.witchy` doc-comments - never hand-edit it.
- [`architecture.md`](architecture.md) - the compile/run pipeline, the file map,
  and the parity discipline between the two backends.
- [`value-model.md`](value-model.md) - the compiled value representation shared
  by lowering, WIR, and the wasmtime runtime.
- [`wasm-abi.md`](wasm-abi.md) - the WebAssembly ABI: the host imports, the value
  representation, and which imports carry authority.
- [`performance.md`](performance.md) - the ownership/clone-elision model and its
  knobs.
- [`binary-distribution.md`](binary-distribution.md),
  [`local-registry.md`](local-registry.md) - packaging and the on-disk registry
  layout.
