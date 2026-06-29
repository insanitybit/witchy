---
rfc: 0019
title: Interactive documentation — the runnable book
status: proposed
created: 2026-06-27
tracking:
---

# RFC-0019: Interactive documentation — the runnable book

## Summary

Turn "The witchy Book" from static, read-only Markdown into a beautiful,
interactive site where **every code example is editable and runnable in the
browser** — compiled and executed by the actual witchy toolchain, client-side,
no server. The compiler already runs in the browser (it powers the standalone
playground and is validated against the interpreter oracle). This RFC wires that
existing engine into the mdBook output, repairs the engine's drifted wiring along
the way, and does a visual pass so the docs look as deliberate as the language.

## Motivation

The book teaches a language whose whole pitch is "run it and see." Today the
reader can only *read* it. The thing that makes witchy click — granting a `main`
its authority, watching a `Dir[Read]` refuse a `write` at compile time, taking a
finite prefix of an infinite generator — lands far harder when the reader edits
the program and re-runs it in place than when they read a frozen snippet.

The pieces are already built; they are just not connected:

- **The engine exists.** `scripts/build-playground.sh` compiles the witchy
  library to `wasm32-unknown-unknown` → `web/witchy.wasm` (3.26 MB after
  `wasm-opt -Oz`). It exports a C ABI (`witchy_alloc`/`witchy_free`/
  `witchy_compile`, plus the intrinsic shims). `web/witchy-host.js` drives it:
  it compiles a snippet to a wasm module and runs that module in-browser. This
  is the real codegen path, not a re-implementation.
- **It is already validated.** `scripts/pg_validate.mjs` loads the very same
  `web/witchy.wasm` + `web/witchy-host.js` and diffs their output against the
  interpreter oracle (`WITCHY_INTERP=1`) for every example. CI builds the wasm
  on every push ("Playground wasm build").
- **Every `witchy` block in the docs is already a complete, correct program.**
  Repo policy (CLAUDE.md) and the `documentation_examples_are_valid` test
  guarantee it: each ```` ```witchy ```` block parses, links, and type-checks,
  and console-only ones run on *both* backends with identical output. Partial
  snippets use an untagged or ```` ```sh ```` fence. So every rendered witchy
  block is a guaranteed-runnable program — the ideal "Run" candidate.

What is *missing* is the connection, and the connection has rotted:

- **The book has no interactivity at all** — `book/witchy-hljs.js` only adds
  syntax highlighting.
- **The standalone playground page is currently broken.** `web/playground.js`
  (loaded by `web/index.html`) still calls `witchy_run`, an export that the
  oracle-only migration *removed* from `src/lib.rs` (the library now exports
  `witchy_compile`). The working compile-and-run path lives only in
  `web/witchy-host.js`, which today is consumed solely by the Node validator —
  not by any browser page. The user-facing playground would throw
  "`witchy_run` is not a function" on first Run.
- **Two highlighters have drifted.** `book/witchy-hljs.js` (a highlight.js
  grammar) and `web/playground.js` (a hand-rolled tokenizer) carry duplicate,
  separately-maintained keyword/builtin lists.

If we do nothing: the marquee web surface for the language stays broken, the
book stays inert, and the only thing keeping the docs honest is a test that
readers never see.

## Design

Four phases. Phase 0 is a prerequisite (repair + de-duplicate the engine);
Phases 1–3 build the runnable book, the beauty pass, and the no-drift guarantee
on top of it.

### Phase 0 — One engine, repaired and shared (prerequisite)

Collapse the two divergent host shims and two highlighters into one set of
shared modules under `web/`, on the *validated* compile path. Per the
break-don't-deprecate convention (pre-prod, one-cut migrations), the stale code
is replaced outright, not kept behind a flag.

- **Host shim:** make `web/witchy-host.js` (`compile` / `runWitchy`, already
  validated by `pg_validate.mjs`) the single source of truth. Delete the dead
  `witchy_run` path from `web/playground.js`; the standalone page imports
  `witchy-host.js` like the validator does. Result: the playground works again,
  and the page, the validator, and the book all run *identical* code.
- **Highlighter:** promote one implementation to `web/witchy-highlight.js`
  (keyword/builtin/type/string-interpolation rules) and have both the book
  (`additional-js`) and the standalone page consume it. The hand-maintained
  keyword lists live in exactly one file.
- **Editor component:** extract the standalone page's editor (a transparent
  `<textarea>` layered over a highlighted `<pre>`, Tab-inserts-spaces,
  ⌘/Ctrl-Enter to run) into `web/witchy-editor.js`, reused by both surfaces.

Net new top-level artifacts: `web/witchy-host.js` (exists), `web/witchy-highlight.js`,
`web/witchy-editor.js`. `web/playground.js` shrinks to page glue.

### Phase 1 — The runnable book

A new `book/witchy-playground.js`, added via `additional-js`, progressively
enhances every `pre > code.language-witchy` block in the rendered book:

1. **Editable cell.** Replace the static block with the shared
   `witchy-editor.js` component, seeded with the block's source. A "Reset"
   control restores the original.
2. **Run + output.** Append a "Run (⌘/Ctrl+Enter)" button and a collapsible
   output pane styled exactly like the standalone playground (`ok` green /
   `err` red).
3. **Lazy, shared wasm.** Fetch `witchy.wasm` (3.26 MB) only on the *first* Run
   on a page, instantiate once, and share that instance across every cell. The
   button shows a "compiling…" state meanwhile. The browser HTTP-caches the
   module, so it is fetched once across the whole book, not per page.
4. **Deep link.** An "Open in playground ↗" control opens the full standalone
   playground pre-filled with the (possibly edited) source via a URL fragment
   (`#code=` + a compact encoding). The book and the playground stay in sync
   because they share the engine.

**Which blocks run, and how each behaves.** Every block is a complete program,
so every block becomes an editor. *Whether it produces output in the browser*
mirrors the existing `documentation_examples_are_valid` classifier exactly,
driven by a generated manifest (Phase 3) rather than re-derived in JS:

- **console-only, non-actor, non-argv** → runs and prints in-browser. (This is
  precisely the set the test already runs on both backends.)
- **needs `Dir`/`Net`/`Clock`/`Env`** → compiles, then errors when it uses the
  capability the browser's deny-by-omission shim never granted. The cell shows
  an inline note framing this as the capability model in action ("the browser
  grants no `Dir`; the program type-checks but the `read` fails — that's the
  point"), not as a bug.
- **intentionally uncompilable teaching examples** (e.g. the `Dir[Read]` that
  calls `write`) → marked *expect-error*; the cell presents the compile error as
  the expected, correct outcome.
- **actor / argv examples** → if the in-browser path can't run them to stable
  output, the cell offers "Open in playground" and keeps the editor live.

### Phase 2 — The beauty pass

- **Cohesive theme.** A custom mdBook theme built around the playground's
  existing palette (Nord-ish tokens, the `✦` accent, ayu-dark default):
  typography, spacing, code-cell chrome, a real hero on the title page.
- **Capability badges.** Render each runnable example's *requested authority*
  (its capability footprint — already computed by `capabilities::analyze` and
  surfaced in the Phase 3 manifest) as small badges on the cell: `Console`,
  `Dir[Read]`, … A witchy-specific flourish that makes "authority is a visible,
  typed artifact" literally visible in the docs.
- **Unified highlighting.** Book and playground render code identically because
  they share `witchy-highlight.js` (Phase 0).
- **Polish.** Copy buttons, refined nav/fold, responsive layout. Optionally a
  small static SVG of the compile pipeline / capability flow (kept tight to
  avoid scope creep).

### Phase 3 — No silent drift (validation + CI + hosting)

The interactive book must never show a reader output that the real toolchain
would not produce.

- **Generated classification manifest.** Extend `documentation_examples_are_valid`
  (it already walks `book/src`, `spec`, and the root docs and classifies every
  block) to emit `book/examples.json`: for each `(file, block-index)`,
  `{ runnable, console_only, expect_error, capability_footprint, expected_output }`.
  A freshness test fails if the committed manifest drifts — the same pattern as
  `stdlib_docs_are_current`. The browser stays dumb: it reads the manifest the
  Rust classifier produced.
- **Headless output check.** Extend `pg_validate.mjs` to load the *shipped book
  wasm* and run every `runnable` book block, asserting its output equals the
  oracle. This closes the loop: a divergence between the in-book run and the
  toolchain fails CI.
- **CI + deploy.** The existing "Playground wasm build" job also drops
  `witchy.wasm` and the shared `web/*.js` into the book output; the book deploys
  (GitHub Pages) with them. Per the project's strict cross-origin-isolation rule,
  every response on the hosted surface carries strict `COOP: same-origin` /
  `COEP: require-corp` / `CORP: same-origin`; we never relax them. (Plain wasm
  does not require isolation today, but the house rule applies to any web
  surface, and it keeps the door open for threaded wasm later.)

### Authoring impact

Authoring stays plain Markdown — no new fence syntax to learn for the common
case, since "complete program" is already the rule. The only new affordance is
an optional directive comment for the rare teaching example whose *expected*
result is a compile error or a capability denial; the manifest generator reads
it so the cell frames the outcome correctly. Everything else is automatic.

## Alternatives

- **Do nothing.** Leaves the flagship web surface broken and the book inert.
  Rejected.
- **Server-backed playground (the `play.rust-lang.org` model).** A backend that
  compiles submitted source. Rejected: witchy already runs *fully client-side*,
  so a server adds hosting cost, a fresh trust/abuse boundary (arbitrary code
  execution), and latency — to deliver strictly less than the in-browser path we
  already have.
- **Link-out only** (every block gets an "Open in playground" link; no inline
  run). This is the cheap first slice and is subsumed by Phase 1 as the
  fallback for actor/argv examples — but as the *whole* design it's a worse read
  (context switch on every experiment). We keep it as a fallback, not the
  destination.
- **Replace mdBook with a custom SPA / Docusaurus / Astro.** Large rewrite that
  discards the working `additional-js` hook and the tight coupling between the
  Markdown sources and `documentation_examples_are_valid`. The interactivity we
  want is achievable as progressive enhancement; the rewrite buys polish we can
  get more cheaply with a custom mdBook theme. Rejected.
- **Per-block JS capability analysis** instead of a generated manifest. Rejected:
  it would re-implement `capabilities::analyze` in the browser and could disagree
  with the authoritative Rust classifier. Generating the manifest from the test
  that already classifies blocks keeps a single source of truth.

## Drawbacks

- **Bundle size.** `witchy.wasm` is 3.26 MB. Mitigated by lazy-loading on first
  Run, sharing one instance per page, and HTTP caching across the book; it is
  never on the critical path for simply *reading*. Further shrinking (splitting
  compiler from runtime, more aggressive `wasm-opt`) is possible later but out of
  scope here.
- **New moving parts to keep honest.** The manifest, the host shim, and the
  shared highlighter are all things that can drift. This is precisely why Phase 3
  makes each one CI-gated (freshness test, headless output diff) rather than
  trusted.
- **Browser ≠ full toolchain.** The browser grants no `Dir`/`Net`/`Clock`/`Env`,
  so some examples can compile but not fully run. We turn this into a teaching
  feature rather than hiding it, but it is a real difference readers must not be
  misled about — hence the inline framing notes.
- **Phase 0 is a hard prerequisite.** The book cannot ship interactivity on top
  of a host shim that throws; the repair must land first.

## Prior art

- **The Rust Book + `play.rust-lang.org`.** mdBook's editable/runnable code
  blocks are the UX we mirror — but Rust ships the work to a server; we run the
  compiler in the reader's browser instead.
- **Compiler-to-wasm playgrounds** (Roc, Gleam, Grain, Elm) — same "ship the
  real compiler as wasm, run client-side" pattern this RFC leans on.
- **Pyodide / PyScript docs** — in-browser execution of the actual language
  runtime inside documentation.
- In-repo: RFC-0007 (the witchy-WASM browser target and its deny-by-omission
  shim), `rfcs/oracle-only-migration.md` (Phase 4 made the playground compile to
  a wasm binary and validate against the oracle — the engine this RFC surfaces),
  and `scripts/pg_validate.mjs` (the validation harness Phase 3 extends).

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
