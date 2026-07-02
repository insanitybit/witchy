---
status: in-progress
note: The single execution-ready index of every ACTIONABLE (proposed/partial) RFC. For an agent picking up implementation. Read the referenced RFC for the design; this file is the concrete, file-anchored plan, ordering, and guardrails. Identity/capability-phase history lives in implementation-roadmap.md; perf history in the round-N plans.
updated: 2026-07-01
---

# RFC execution plan

This is the operational companion to the RFC corpus: for each RFC that still has
work, the concrete entry points, ordered steps, definition of done, size, and
risks — distilled so an implementing agent can start without re-deriving the map.
The **design rationale lives in the RFC**; this file is the *how* and the *order*.

Implemented RFCs are not listed (they are frozen records). The full status audit
that produced this list is reflected in each RFC's `status:` field (corrected
2026-07-01).

## Global rules (read first — these are hard)

- **Parity is the one rule.** Every observable behavior must work — or loudly
  error — identically on the interpreter (`crates/witchy-interp`) and the compiled
  WASM tier (`crates/witchy-lower` → wasmtime). Add a differential test in
  `src/example_tests.rs`, and a runnable `book/` example for anything user-visible.
- **The gate is `./scripts/check.sh --fast`** (build + clippy `-D warnings` +
  `nextest --workspace`). It must be green before every commit; `--full` before a
  push (note: `--full` is currently blocked by a *pre-existing* `coven_audits` 403
  e2e flake unrelated to this work — use `--fast` for the commit loop).
- **The PATH `witchy` is the RELEASE binary; `cargo build` writes DEBUG.** Run
  `./target/debug/witchy …` or `cargo build --release` before testing via `witchy`.
- **Optimizations generalize — never special-case a method** (no new `*_cap`
  helpers / `self_*` recognizers). Not relevant to most tracks here, but binding
  for the perf track.
- **Security-sensitivity:** the capability tracks (0038/0039/0005/0020/0011) touch
  the authority model. If a design point is not resolvable from the RFC, *surface
  it, don't guess* — a wrong capability decision is a security bug.
- **Git hygiene:** commit as configured with the `Co-Authored-By` line; the working
  tree may be shared — revert with `git checkout -- <file>`, never `stash`/`reset
  --hard`/`clean`; branch off master; push only on request.

## Priority & dependency order

Current direction (2026-07-01): **step back from performance, focus on
language/feature RFCs.** So:

```
TRACK A — language/features (DO FIRST)
  0038 grantable user caps            [SHIPPED]
  0039 Glamour capability-safe effects [SHIPPED]
  0011 refinement remainder (Dir/File policies + carried-state) [SHIPPED]
  0019 interactive documentation                                  [independent]

TRACK B — security/net (as capacity allows)
  0005 unforgeable capabilities (externref)      [large, ABI cut]
  0020 rebinding-resistant HTTP (resolve/pin)    [independent]

TRACK C — performance (DEFERRED by direction)
  0031 SIMD · 0034 residual (L5/L6) · 0036 residual (executor arena-reset)
```

**RFC-0038, RFC-0039, and RFC-0011 are SHIPPED** (`status: implemented`) — Track A's
capability work (grantable root caps + Glamour's token-gated effects with coven-web
migrated + Dir/File entry policies incl. `kind`, `restrict` retired) is done. Next
pickup: **RFC-0019** (the last Track A item), then Track B (**RFC-0005**, **RFC-0020**).

---

## TRACK A — language / features

### RFC-0041 — the docs as a glamour app (runnable book, self-hosted) — SUPERSEDES 0019
- **Goal:** the book becomes a **witchy program** — a glamour app compiled to wasm,
  static-hosted, client-side, no server — that renders `book/src/*.md` via
  `markdown.to_vnode` with `SUMMARY.md`-driven nav, and turns every `witchy` code block
  into an editable/runnable cell backed by the browser compiler. The ultimate dogfood.
- **Depends on:** nothing new (every piece ships: glamour capability-safe/routing/root
  ABI, `std/markdown`, the browser compiler, coven-web as the proven pattern).
- **Entry points:** NEW `projects/docs/` (glamour app, coven-web shape); content
  `book/src/*.md` + `book/src/SUMMARY.md` (unchanged authoring); renderer
  `projects/glamour/src/markdown.witchy` (`to_vnode`); host shell
  `web/witchy-runtime/glamour-dom.mjs`; engine `web/witchy.wasm` + `web/witchy-host.js`
  (`compile`/`runWitchy`); **broken** `web/playground.js` (calls removed `witchy_run`);
  validator `scripts/pg_validate.mjs`; manifest source `src/example_tests.rs`
  `documentation_examples_are_valid`; remove `book.toml` + `scripts/build-book.sh`
  mdbook path + `.github/workflows/ci.yml` mdbook job.
- **Phases:**
  - **P0 (prereq, kept from 0019) — DONE:** the shared highlighter `web/witchy-highlight.js`
    (pure `source→HTML`, node-tested by `witchy_highlighter_colours_current_syntax`, corrected
    list: retired `restrict`/`subdir` dropped, `capability`/`grantable`/`only`/`deny` added).
    And the standalone playground is REPAIRED: `web/playground.js` is now an ES module that runs
    snippets via the shared, oracle-validated `witchy-host.js` `runWitchy` (the dead `witchy_run`
    path is gone) and colours with `witchy-highlight.js`; its EXAMPLES were rewritten to CURRENT
    witchy (the old ones used removed `int_to_string`/`<>`/`restrict`) and are gated by
    `playground_examples_are_current_witchy` (5 compile+run; the `Dir[Read]`-writes example is a
    deliberate, asserted compile error). (The editor overlay stays inline in `index.html` — not
    extracted to a separate `witchy-editor.js`; the docs cells use the simpler textarea in
    `buildRunnableCell`. NOTE for P2, since resolved: the docs app renders code as XSS-safe
    VNodes, so highlighting there is a host-side concern — deferred, not blocking.)
  - **P1 — DONE (`projects/docs/src/docs.witchy`):** the glamour docs-app shell — a
    capability-pure MVU app that fetches `/content/SUMMARY.md`, parses its nav IN WITCHY
    (`parse_nav`), renders each routed page via `markdown.to_vnode`, and navigates with
    `UiRoute`. Empty host footprint; narrows `UiRoot`→GET `UiFetch`+`UiRoute`. Proven by
    `glamour-docs.test.mjs` + `glamour_docs_app_renders_book_pages` (nav from SUMMARY,
    page fetch+render, Markdown safety). NOTE: nav is fetched+parsed at RUNTIME (simpler +
    more dogfood than the RFC's build-step nav.json); a build step can still emit the
    content bundle in P3. Remaining P1 polish: nested nav (currently flat), a real
    `/content/` build (copy `book/src/*.md`), CLI convenience.
  - **P2 — design resolved; markdown hook DONE:** `markdown.to_vnode` now tags fences with
    `<code class="language-witchy">` (inert escaped text — proven both backends by
    `markdown_code_fence_carries_its_language_class_on_both_backends`), the hook the host uses
    to find runnable blocks. **DESIGN (refined, supersedes the RFC's Compartment sketch):** a
    runnable cell is progressive-enhancement in the MAIN frame, NOT a `Compartment` — a
    reader's snippet compiles to a capability-DENIED pure-compute wasm (deny-by-omission), so
    it's already contained; a compartment's `connect-src none` would only block loading the
    compiler. Highlighting is host-side over the escaped `language-witchy` text (XSS-safe; no
    sink in the rune). **ENHANCER DONE:** `web/witchy-runnable.js` —
    `enhanceRunnableCells(root, {document, loadCompiler})` finds `pre>code.language-witchy`,
    adds a host-managed Run button + output pane (idempotent, DOM-agnostic — no innerHTML/
    querySelector), and on Run compiles+runs via `witchy-host.js` + `web/witchy.wasm`. Proven
    END-TO-END headlessly by `witchy-runnable.test.mjs` (a FakeElement harness drives Run,
    asserts the real output; compile error → error cell; idempotent; non-witchy fences
    untouched), wrapped by `witchy_runnable_cell_compiles_and_runs_in_page`. **WIRING FINDING
    (tested):** calling the enhancer from an `afterRender` hook does NOT work — the enhancer
    MUTATES the DOM (wraps `<pre>` in a cell), and glamour's next re-render (e.g. GotNav after
    GotPage) diffs against that mutated DOM and corrupts it. A no-op `afterRender` renders fine;
    the real enhancer breaks the page. So progressive DOM-mutation conflicts with glamour's
    framework-owned diffing — my "no Compartment needed" refinement was WRONG about the DOM
    ownership (right about the security: no iframe needed). **HOST-SLOT MECHANISM DONE** (the
    fix): glamour has a new `Slot(kind, data)` VNode — a subtree the host mounts once via a
    registered `opts.slots[kind]` renderer (main frame, no iframe) that glamour NEVER diffs
    into. Additive (variant + ctor + to_html/node_json; host `isSlot`/`mountSlot`/patch-keep/
    kindOrTagChanged); wire parity `glamour_slot_wire_is_identical_on_both_backends`; the core
    non-diff property proven by `glamour_slot_is_a_non_diffed_host_subtree` (a host widget +
    its mutation SURVIVE a re-render; the renderer isn't re-called). All 16 glamour drivers
    still green. **WIRING DONE — the runnable book is real end-to-end:** `web/witchy-runnable.js`
    now exposes `buildRunnableCell(doc, source)` + `runnableSlot(opts)` (reusing the tested cell
    logic; the standalone `enhanceRunnableCells` uses them too); the docs app's `runnable_markdown`
    remaps each `witchy` fence in `markdown.to_vnode`'s output to `glamour.slot("witchy-runnable",
    code)`; and the docs mount registers the slot renderer. Proven by `glamour_docs_app_renders_book_pages`
    (extended): a page's fence becomes a runnable cell AND the page still renders + navigates —
    the non-diffed slot means the re-render corruption is gone. So a book code block compiles +
    runs real witchy in-page, glamour-clean. **P2 CORE COMPLETE.** Editable cell DONE:
    `buildRunnableCell` now renders a `<textarea>` seeded with the source, Run executes the
    reader's EDITED code (proven by `witchy-runnable.test.mjs`: edit → the edited output),
    ⌘/Ctrl-Enter runs — the "editable AND runnable" headline. (Kept inline in
    `buildRunnableCell` rather than a separate `witchy-editor.js`; the highlight-overlay + tab
    behaviour of the standalone playground editor is the only piece not shared, deferred.)
    Remaining polish: capability badges from `examples.json`, theme.
  - **P3 (kept from 0019) — manifest DONE:** `book/examples.json` is generated from the
    SAME classifier as `documentation_examples_are_valid` (per block: runnable/console_only/
    expect_error/right-precise footprint/interpreter output) and freshness-gated by
    `book_examples_manifest_is_current` (the `stdlib_docs_are_current` pattern; regen with
    `BLESS_EXAMPLES=1`). **HEADLESS DRIFT GATE DONE:** `scripts/validate_book_examples.mjs`
    runs every `runnable` block through the SHIPPED browser wasm (`web/witchy.wasm` +
    `web/witchy-host.js`) and asserts its output equals the manifest oracle — closing the loop
    (browser == manifest == interpreter). Verified locally against a FRESH wasm: 90 agree, 0
    diverge, 43 non-runnable. (The earlier local divergences were a STALE `web/witchy.wasm`
    artifact — a rebuild fixed all of them; there is no browser std-subset gap.) Wired into the
    CI `playground` job (stage the wasm → run the validator), NOT the Rust gate (the wasm is a
    gitignored artifact that would go stale-flaky). **BUNDLE DONE:** `scripts/build-docs.sh`
    assembles the deployable static site — the docs app compiled to wasm + the real `book/src`
    content under `content/` + the flat web modules + `web/docs.html`/`docs-boot.js`/`docs.css`
    + the manifest + the browser compiler. Validated against the REAL book by
    `glamour_docs_bundle_renders_the_real_book`: the bundle renders the actual book (32-page nav
    from the real SUMMARY.md, real pages, a real fence → an editable cell). REMAINING: a CI job
    that builds + deploys the bundle to GitHub Pages with **strict COOP/COEP/CORP**, and remove
    mdBook (`book.toml`, the `build-book.sh` mdbook path, the CI `book` job).
- **DoD:** the docs are a glamour app (empty host footprint) rendering every page via
  `markdown.to_vnode` with SUMMARY nav; console-only cells run in-browser and agree with
  the oracle (extended `pg_validate`); capability badges + expect-error framing from the
  manifest; `examples.json` freshness test + headless-diff CI + strict cross-origin
  headers; mdBook removed; RFC-0019 superseded. **Size:** L (P0 unblocks; search is the
  named gap — a small client-side index in P3 or a follow-up). **Known cost:** rebuilding
  the reader shell mdBook gave free (nav/layout/**search**).

---

## TRACK B — security / net

### RFC-0005 — unforgeable capabilities (externref)
- **Goal:** stop capability handles being forgeable via intra-guest memory
  corruption — move from i32 handles to wasmtime `externref`.
- **Exploitability:** a **real** intra-guest gap (an ownership-analysis false
  negative → heap corruption → forged handle), not just defense-in-depth; the
  wasmtime sandbox does not protect guest-internal memory.
- **Entry points:** i32 handle tables in `crates/witchy-runtime/src/runtime.rs`
  (`dirs`/`nets`/`secrets` ~:239–276), the `signing@0` fallback `:2218`; import
  signatures `crates/witchy-wir/src/wir_prelude.rs:257` (`IMPORT_COUNT`); lowering
  `crates/witchy-lower/src/codegen/mod.rs`; wasmtime `Config` `runtime.rs:13`.
- **Ordered steps (low-risk first):** (1) trap on in-place writes (parity with
  interp); (2) delete the `signing@0` fallback + require explicit secret grants;
  (3) choose the aggregate/closure representation (RFC recommends GC structs);
  (4) enable `wasm_reference_types`/`wasm_gc`; (5) rewire host imports to
  `externref` cap args + downcast to backing grant; (6) lower caps to `externref`
  in codegen; (7) fuzz the ownership analysis; (8) attenuation test suite.
- **DoD:** all cap imports take `externref`; parity green; no bypass in the bounded
  threat model; fuzzer finds no diffs. **Size:** L (a coordinated ABI cut — cannot
  coexist with the i32 ABI). **Guardrail:** steps 1–2 ship value immediately and are
  independently committable; do them first even if the full externref cut waits.

### RFC-0020 — DNS-rebinding-resistant HTTP
- **Goal:** resolve-once-and-pin HTTP: `net.resolve` + `net.connect_pinned` + a
  sealed `PinnedUrl`, so user-supplied-URL fetch is SSRF/rebinding-safe.
- **Status:** Layer 0 (resolve-once invariant) + Layer 1 (`confine.private()`)
  shipped (`std/confine.witchy:59`); Layers 2–3 missing.
- **Entry points:** `crates/witchy-caps/src/capabilities.rs:150` (IPv4 CIDR — IPv6
  gap here); host ops template `crates/witchy-runtime/src/runtime.rs:1975`
  (`host_net_connect`) + `wir_prelude.rs:282` `IMPORT_COUNT`; `std/http.witchy`.
- **Ordered steps:** (1) add IPv6 CIDR parse/match (closes a silent `confine.private()`
  gap); (2) `net.resolve` host op (returns `List(String)` of IPs, gated on
  `Net[Connect]`); (3) `net.connect_pinned` (literal-IP dial, re-checks allowlist,
  host used for SNI/Host); (4) sealed `PinnedUrl` type + `resolve`/`get_pinned`;
  (5) wire into `std/http` (`get_pinned`/`pin`/`pin_with`); (6) differential test
  with a loopback listener + a rebinding-mock resolver (assert no second resolve).
- **DoD:** IPv6 ranges match; pinned connect uses the resolved IP with correct
  Host/SNI; a rebinding attack is provably prevented (resolve-once holds); both
  backends agree. **Size:** M–L. **Guardrail:** the chooser closure must round-trip
  identically on both backends; step 1 (IPv6) is a standalone S fix worth doing now.

---

## TRACK C — performance (DEFERRED by current direction)

Listed for completeness; not the current focus. The kernel-timing benchmarks
(`benchmarks/`, `bench/`) localize the remaining gap to **tight scalar/numeric
loops** — that is what these target.

- **RFC-0031 — SIMD for stdlib hot loops** (open, high ROI, L). No SIMD exists yet
  (verify: no `v128` in `crates/`). Target: `wasm-simd` in codegen numeric-loop
  lowering + byte-at-a-time stdlib primitives. The lever for nsieve/fannkuch/
  mandelbrot. wasmtime already supports SIMD.
- **RFC-0034 (residual)** — L1–L3 shipped (wasm-opt, bounds-elide, closure-devirt).
  Residual: L5 worker-VM pooling (low value), L6 representation specialization
  (SIMD part subsumed by 0031). M, low priority.
- **RFC-0036 (residual, NOT a blocker)** — Design B (iterative scheduler) shipped in
  `std/task.witchy`. Residual: per-iteration **arena reset** in the executor loop to
  bound the ~1.1M-cell leak (chan_throughput caps at ~8–9k messages). Memory-perf,
  decoupled from safety. M.

---

## Maintenance

When an RFC here is implemented: flip its `status:` to `implemented`, move the
current behavior into `spec/` + code (the RFC freezes), and delete its section from
this file. This file tracks only *open* work.
