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

**RFC-0038, RFC-0039, RFC-0011, RFC-0041 (supersedes 0019), and RFC-0020 are SHIPPED**
(`status: implemented`) — Track A's capability + docs work and Track B's
rebinding-resistant HTTP (resolve/pin/`connect_pinned` + sealed `PinnedUrl`, IPv6 CIDR
matching) are done. Next pickup: **RFC-0005** (unforgeable capabilities / externref) —
the last open Track B item.

---

## TRACK A — language / features

**COMPLETE.** RFC-0038, RFC-0039, RFC-0011, and RFC-0041 (supersedes 0019) all shipped
(`status: implemented`) and are frozen. The witchy docs are now a client-side glamour app
deployed to GitHub Pages; mdBook is removed. Follow-up polish (not blocking): docs search
+ capability badges.

---

## TRACK B — security / net

### RFC-0005 — unforgeable capabilities (externref)
- **Goal:** stop capability handles being forgeable via intra-guest memory
  corruption — move from i32 handles to wasmtime `externref`.
- **Exploitability:** a **real** intra-guest gap (an ownership-analysis false
  negative → heap corruption → forged handle), not just defense-in-depth; the
  wasmtime sandbox does not protect guest-internal memory.
- **Status:** the independently-shippable HARDENING is DONE — (1) **trap on in-place
  writes**, COMPLETE across every in-place-append fast path: `list_push_cap`,
  `str_append_cap`, and `dict_insert_cap` bounds-check the write against the buffer's
  real rc allocation (`[ptr-4]` for lists/strings, `[d-8]` for the offset dict pointer)
  and trap, converting a silent-corruption false-negative into a loud parity-identical
  error — each proven by a `*_traps_on_overstated_cap` positive test plus the whole
  suite/fuzzer staying green; `list_set_cap`/`list_update_cap` were already guarded by
  `index < len`. (2) **`signing@0` fallback deleted** (the signing key is now a real
  granted `"signing"` secret, no magic index — `signing_at_zero_fallback_is_removed…`).
  (7, RFC numbering) **wasmtime proposal surface shrunk** (unused proposals off, Spectre
  mitigations documented-on). Step 8 (attenuation-rule tests) was ALREADY comprehensive
  (`dir_rights_are_statically_enforced`, `net_capability_cannot_escalate`,
  `net_*_enforced_at_instantiation`, `as_ascription_narrows_to_subsets_only`). A
  differential fuzzer already exists (`metamorphic_property_laws` — it caught a real
  false-trap during step 1). The externref ABI and fixed-layout GC aggregate/closure
  core are now implemented. Remaining representation gates are the explicitly
  rejected generic containers/fields, region copy-out, and isolated typed callbacks.
- **Entry points:** i32 handle tables in `crates/witchy-runtime/src/runtime.rs`
  (`dirs`/`nets`/`secrets`); import signatures `crates/witchy-wir/src/wir_prelude.rs`
  (`IMPORT_COUNT`); lowering `crates/witchy-lower/src/codegen/mod.rs`; wasmtime `Config`
  `runtime.rs` (hardened).
- **Implemented core:** approach (A) GC structs/arrays; `wasm_reference_types` and
  `wasm_gc`; host-import `externref` grants; exact reference kinds across direct and
  indirect calls; uniform GC closure wrappers with per-lambda typed environments;
  fixed-layout nominal/tuple function fields; and direct `List(fn(...))` GC arrays.
- **DoD:** all cap imports take `externref`; parity green; no bypass in the bounded
  threat model; fuzzer finds no diffs. **Size:** L (a coordinated ABI cut — cannot
  coexist with the i32 ABI; best done as a dedicated focused effort, not folded into a
  loop iteration). The hardening steps (1/2/7) shipped independently first, as planned.

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
