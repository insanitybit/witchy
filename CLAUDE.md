# Working in this repo (agent notes)

`witchy` is a capability-secure language with twin backends: the interpreter
(`crates/witchy-interp/src/interpreter.rs`) is the reference, the compiled-WASM
path (`crates/witchy-lower/src/codegen.rs`) must match it. **Authoritative docs:**
`CONTRIBUTING.md` (build/test/parity) and `spec/architecture.md` (pipeline +
workspace layout). Read those first; this file only adds the things that have
actually bitten agents.

**The compiler is a Cargo workspace** (RFC-0018): seven stage crates under
`crates/` (`witchy-syntax`/`-types`/`-wir`/`-lower`/`-runtime`/`-interp`/`-caps`)
plus the `witchy` binary. Build/lint/test the whole thing with `--workspace`
(`cargo nextest run --workspace`, `cargo clippy --workspace --all-targets`). The
root lib re-exports every crate's modules, so `crate::{ast,typeck,codegen,…}::…`
paths still resolve from the binary — but new code belongs in the owning crate.

## Gotchas

- **The `witchy` on your PATH is the RELEASE binary; `cargo build` produces
  DEBUG.** `~/.cargo/bin/witchy` symlinks to `target/release/witchy`, but
  `cargo build` writes `target/debug/witchy`. So editing `src/`, running
  `cargo build`, then testing with `witchy foo.witchy` runs your *old* code and
  looks like your change did nothing. Either run `./target/debug/witchy …`, or
  `cargo build --release` to refresh the PATH binary. (Check with
  `which witchy` vs. your build output path if results look stale.)
- **`spec/stdlib.md` is GENERATED — never hand-edit it.** It is rendered from
  the doc-comments in `std/*.witchy`; a test (`stdlib_docs_are_current`)
  fails if it drifts. Edit the `std/*.witchy` comment, then regenerate:
  `witchy doc std/*.witchy > spec/stdlib.md`.
- **`book/` and `README`/`spec` ` ```witchy ` blocks are executed tests.** A
  fenced `witchy` example must be a complete, correct program (it is parsed,
  type-checked, and run on both backends). Use an untagged or ` ```sh ` fence
  for partial snippets. Don't document an API that doesn't exist — the build
  won't catch a *prose* claim, but it's how stale docs like the old
  `derive(Json)` (which never existed; use `derive(Reflect)` + `json.stringify`)
  slip in.

## The one rule: parity

Two backends, **zero silent divergence**. Any observable behavior must work
(or loudly error) identically on both. Add a differential test in
`src/example_tests.rs` and, for anything user-visible, a runnable `book/`
example. There is no CI: `./scripts/check.sh` is the green gate (build + clippy
`-D warnings` + `nextest --workspace` + the wasm build) — run it before every
commit, and `--full` before a push.

## Trait-method dispatch (recently extended)

`show(x)` / `less(x, y)` etc. resolve by recovering the receiver's concrete type
from the argument's shape (`head_type_name` / `recover_generic_call` in
`crates/witchy-types/src/traits.rs`). This now includes call results — `list.at(xs,i)`, `xs[i]`, and
generic functions whose return is a type var. If a fresh trait call still won't
resolve, the type error guides you (`${x}` / `say` / a typed param), and the
fix belongs in `recover_generic_call`, not a workaround.
