# Contributing to witchy

## Build and test

`./scripts/check.sh` is the green gate — **run it before every commit.** It runs
the whole workspace through build, clippy (deny-warnings), the test suite, and
the wasm playground build, in that order, and is the single source of truth for
"the project is healthy". `--full` adds the from-scratch e2e acceptance test.

```sh
./scripts/check.sh          # build + clippy + tests + wasm build
./scripts/check.sh --full   # the above, plus ./scripts/e2e-full.sh

# Or run one piece while iterating on it:
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
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
`--quick` skips its test stage, which is redundant once `check.sh` has run the
suite.

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
error paths too) — and with the **same complete diagnostic** (RFC-0045): a
runtime abort (out-of-bounds index, integer division/modulo failure,
`string.to_int` junk, `NaN` ordering, `fail(msg)`) carries the interpreter's
exact text on the compiled backend via the always-linked, authority-free
`__witchy_abort` import. The harness compares that diagnostic byte-for-byte,
including lexical function and source line. Improving an abort's wording is
therefore a shared-template change in
`crates/witchy-syntax/src/diag.rs` (`DiagTemplate`) plus the pinned browser mirror
— never edit one backend's formatter alone. When debugging a compiled trap, set
`WITCHY_WASM_BACKTRACE=1` to dump the full named-frame wasm backtrace beneath the
message (the message itself always prints).

## Formatting

Rust code is **hand-formatted** — do NOT run `cargo fmt` (it reformats ~71
files and fights the intended style; the gate deliberately excludes rustfmt).
witchy code (std/, examples/, projects/): `witchy fmt <file>` — CI runs
`witchy fmt --check` over the tree, the only formatting gate. If you edit `std/`, regenerate
the API reference: `witchy doc std/*.witchy > spec/stdlib.md` (a test asserts it
is current).

## Witchy house style

Witchy source in this repository follows the executable
[Idiomatic witchy](book/src/idioms.md) chapter. In particular:

- use interpolation for presentation strings, not concatenation;
- use statement-form mutators such as `out.push(x)` and `d.insert(k, v)`, not
  reassignment through `list.push` or `dict.insert`;
- represent an absent value with `Option`, but a failed operation with `Result`
  and propagate it with `?`; never use `Some(String)` to mean failure;
- destructure tuple-valued iteration in the `for` binder;
- prefer comprehensions and `list`/`iter` combinators when they make the data
  flow clearer than an index-threaded loop;
- use standard-library helpers instead of private wrappers with the same
  contract; and
- spell capability operations as methods on the capability value.

Explicit concatenation remains appropriate when constructing a byte-exact
protocol payload, cache key, generated source file, or similar format. Mark
such a site with `// idiom-exempt: <reason>` so review can distinguish a format
contract from presentation text. The formatter enforces layout; reviewers
enforce these semantic idioms.

## Documentation is tested

Runnable ` ```witchy ` examples in the markdown docs (`README.md`, `spec/*.md`,
`book/src/*.md`) are exercised by the runnable-book gate,
`scripts/validate_book_examples.mjs` (run in CI). It loads the compiled
playground engine (`web/witchy.wasm` + `web/witchy-host.js` — the same engine a
reader's Run button uses), and for each entry in the `book/examples.json`
manifest it locates the referenced ` ```witchy ` block, runs the ones marked
`runnable`, and asserts the block's output equals the interpreter output the
manifest recorded — so an in-book run can never diverge from the toolchain that
produced the manifest. A divergence, or a manifest block that no longer exists,
fails the gate. So a runnable example in the docs must be a **complete, correct
program** — when you change the language, the examples that demonstrate it fail
the build until updated. Genuinely partial snippets (signatures with `...`, shell
commands, sample output) use a different fence (untagged, or ` ```sh `); blocks
the manifest marks non-runnable are recorded but not executed.

## Where things live

See [spec/architecture.md](spec/architecture.md) for the pipeline and the
workspace layout (the compiler is split into stage-aligned crates under
`crates/`). Quick orientation: the interpreter
(`crates/witchy-interp/src/interpreter.rs`) defines semantics; codegen
(`crates/witchy-lower/src/codegen/`) must match it; typeck
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

Terminology in docs and diagnostics: a **capability** is the unforgeable
value (`Console`, `Dir`); a **right** is a permission within one
(`Dir[Read]`); a **verb** is an operation checked against rights (`read`,
`connect`). Don't use the three interchangeably.

## Adding a semantic

For a new value type, binary operator, builtin, host import, or runtime trap,
make the change across the whole pipeline in one commit series:

1. syntax/AST and formatter if the surface changes;
2. type checking, including capability rights and trait/protocol obligations;
3. interpreter semantics, because it is the oracle;
4. lowering, WIR kind/layout helpers, and wasm encoding;
5. runtime host import or trap plumbing when authority or host state is involved;
6. shared diagnostics/templates for any new error text;
7. differential coverage in `src/example_tests.rs` or the fuzzer;
8. spec/book examples, plus generated docs if `std/` changes.

Use [spec/value-model.md](spec/value-model.md) as the compiled representation
checklist. If the new semantic does not fit that table, update the table and
make both backends prove the new representation with tests.

## Generated and derived docs

Treat generated and derived documentation as build artifacts with source of
truth elsewhere:

- `spec/stdlib.md` is generated from `std/*.witchy` doc comments. Do not
  hand-edit it; update the source comments or generator and regenerate it.
- `wiki/` is derived, disposable synthesis over code, `spec/`, `rfcs/`, and
  `external-refs/`. Regenerate it instead of hand-maintaining pages.
- `external-refs/` is curated research input, not the current Witchy spec. Keep
  notes attributed and use RFCs/spec docs for Witchy decisions.

If a public-facing docs change updates commands, examples, or generated output,
run the command or generator named by the doc and include that command in the PR.

## Internal and operator-facing artifacts

`CLAUDE.md`, `.claude/skills/**`, `OVERNIGHT_REPORT.md`, and similar local
operator notes are useful working material, but they are not automatically public
contributor documentation. Do not delete, publish-sanitize, or rewrite them as
part of unrelated product docs. If the repository needs a public/open-source cut,
handle those files in a dedicated policy PR that chooses one outcome explicitly:
keep them, move them out of the public branch, preserve them privately and delete
from a mirror, or convert selected instructions into public docs here.

## License

Dual MIT / Apache-2.0. By contributing you agree your work is licensed the
same way.
