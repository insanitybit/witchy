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

### RFC-0019 — interactive documentation (runnable book)
- **Goal:** wire the existing browser witchy engine into the mdBook so doc code
  blocks become runnable/editable; validate against the oracle in CI.
- **Depends on:** nothing (engine exists; it's integration).
- **Entry points:** book `book.toml`, `book/src/*.md`, `book/witchy-hljs.js`; engine
  `web/witchy.wasm` + `web/witchy-host.js` (working: `compile`/`runWitchy`);
  **broken** `web/playground.js` (calls the removed `witchy_run`); validator
  `scripts/pg_validate.mjs`; manifest source `src/example_tests.rs:3255`
  `documentation_examples_are_valid`; `.github/workflows/ci.yml` book/playground jobs.
- **Phases:**
  - **P0 (prereq):** fix `playground.js` to use `witchy-host.js`; dedupe the
    highlighter into one `web/witchy-highlight.js`; extract a reusable editor
    (`web/witchy-editor.js`).
  - **P1:** generate `book/examples.json` from `documentation_examples_are_valid`
    (per-block: runnable?/console-only?/expect-error?/footprint/expected-output);
    add `book/witchy-playground.js` (progressive enhancement of `code.language-witchy`
    into runnable/editable cells, lazy-load wasm on first Run); wire `book.toml`.
  - **P2:** theme + capability badges + polish.
  - **P3:** extend `pg_validate.mjs` + CI to diff book output vs oracle; a manifest
    freshness test (mirror `stdlib_docs_are_current`); **strict COOP/COEP/CORP** on
    the deployed book (house rule).
- **DoD:** book cells run in-page and agree with the oracle in CI; `documentation_examples_are_valid`
  stays green; strict cross-origin-isolation headers set. **Size:** L (P0 is the
  unblocker; P3 is the no-drift guarantee).

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
