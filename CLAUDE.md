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

**Where write-ups go:** **bug reports → `bugs/`** (a gitignored local backlog — one
`BUG-NNN-slug.md` per defect: symptom, repro, root cause, fix). **Design decisions and
proposals → `rfcs/`** (tracked, numbered `NNNN-slug.md`, status lifecycle per RFC-0001).
Don't file a bug as an RFC or an ad-hoc design doc as a bug; security findings still go to
`security-eval/` (SEC-NNN, also gitignored).

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
example. CI exists (`.github/workflows/ci.yml`: build/clippy/nextest, heap-check
fuzz, parity sweep, e2e, docs, fmt), but the local green gate is
`./scripts/check.sh` (build + clippy `-D warnings` + `nextest --workspace` + the
wasm build) — run it before every commit, and `--full` before a push.

## Optimizations generalize — never special-case a method

Memory / in-place / reclamation optimizations are **driven by the ownership
conventions** (`let`/`var`/`own`, and `frozen`/`unique`) plus the escape/uniqueness
analysis — **one general mechanism for ALL operations.** Do **not** add a
per-method fast path: no new `*_cap` runtime helpers (`dict_insert_cap`,
`list_push_cap`, …) and no new per-method `self_*` recognizers in
`crates/witchy-lower/src/analysis.rs` (`self_insert_args`, `self_set_at`, …). A
per-operation hack does not generalize — every new method would need its own code,
and the ones that don't get it silently regress (that is exactly why `dict.remove`
leaked: it had no `dict_remove_cap`). The conventions already express the ownership
fact (a unique `var` may be mutated/reclaimed in place; a `let` may not escape);
consume that fact uniformly. The existing `*_cap` + `self_*` family is **retained,
not deleted**: RFC-0051 (I3) measured removing it and found the general path
perf-negative — it OOM-traps several benchmarks — so the family is load-bearing.
The forward rule still stands: add **no new** per-method fast paths; the general
mechanism must absorb every *new* operation. (RFC-0016 is the general reclamation
floor; RFC-0051 is why the existing zoo stays.)

## Trait-method dispatch (RFC-0046: typed, table-first)

`show(x)` / `less(x, y)` etc. resolve by reading the receiver's concrete type
from **typeck's `TypeTable`** — the real inference judgment, not a string guess
(`Ctx::type_name` / `Mono::type_name` in `crates/witchy-types/src/traits.rs`,
`table_scope_name` first). RFC-0046 deleted the string "shadow type system"
(`recover_generic_call`, `bind_type_var`, `builtin_ret`): call results
(`list.at(xs,i)`, `xs[i]`, generic returns) are typed by the checker and the
annotate/mono **fixpoint** (`lower_with`), which re-annotates after each round so
a generic helper's bounded call (`iter.collect`) resolves once the helper is
specialized. **A fresh dispatch fix belongs in the typed path** — make the
checker type the expression (a `call_sig` entry, a signature), so the table
carries it — never in a new string-shape table. If a trait call still won't
resolve, the type error guides you (`${x}` / `say` / a typed param). The empty-
table **quiet pre-mono pass** still uses `head_type_name` for local judgment
(literals/ctors/params) and `cap_op_return_type` for chained cap-op results
(bare intrinsics the checker types but the empty table can't surface); those are
the documented residual, not an invitation to grow the shape tables.
