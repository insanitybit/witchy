---
rfc: 0091
title: "Browser-runnable capability examples — real Clock/Env, an in-memory Dir (Net deferred)"
status: implemented
created: 2026-07-10
supersedes:
superseded-by:
tracking: >
  Phase 1 ACCEPTED and IMPLEMENTED (2026-07-15): an OPT-IN browser capability
  host (web/witchy-runtime/witchy-runtime.mjs) backs Clock (real wall/monotonic
  time), Env (empty by default + page-supplied immutable map) and Dir (a confined
  per-run in-memory tree). The default host stays capability-DENIED — the opt-in
  host is an explicit teaching/playground surface, never an ambient widening.
  Net is DEFERRED to a later phase for a concrete ABI reason (below): the
  synchronous host-call ABI cannot compose with async fetch, and fetch is not a
  socket. Exec/Secret stay un-runnable by design. Measured against the current
  book/examples.json (164 blocks): 115 Console-only already run; the classifier
  now also marks 134 blocks `browser_runnable` (+19: Dir/Env/Clock). The docs
  book cell itself still uses the capability-denied engine, so those examples run
  only under an embedder that wires in the opt-in host.
---

# RFC-0091: Browser-runnable capability examples

The host implementation is in [`web/witchy-runtime/witchy-runtime.mjs`](../web/witchy-runtime/witchy-runtime.mjs),
with parity coverage in [`tests/browser/shim.rs`](../tests/browser/shim.rs) and
[`web/witchy-runtime/capability-host.test.mjs`](../web/witchy-runtime/capability-host.test.mjs).

## Problem

The docs app (this book) turns fenced `witchy` blocks into editable, executable
cells that compile to WebAssembly and run on the browser's own engine. A cell is
made runnable only when its program is **`Console`-only** (`example_tests.rs`:
`console_only = footprint.total.keys().all(|k| k == "Console")`); everything else
is served as static, un-runnable code because `compile_source` links every
authority import as a **trapping stub** ("the browser has none", `src/lib.rs`).

Measured against the current `book/examples.json` (**164 blocks**): **115 run**
(Console-only), **49 do not**. Of the 49, the non-Console footprints are
predominantly `Dir` (17), then `Net` (9 across families), `Clock` (3), `Env` (3),
`Secret`/`SecretStore`/`Exec` (1 each) and 14 capability-free non-`main` snippets.
A reader learning file I/O or time can read the code but never press Run. Phase 1
of this RFC makes the `Clock`/`Env`/`Dir` subset runnable under an opt-in host —
the classifier marks **134** blocks `browser_runnable` (+19 over the 115
`Console`-only) — leaving `Net` (deferred), `Exec`, and `Secret` un-runnable.
(Counts are the measured manifest at implementation time; they drift as the book
grows, so treat them as a snapshot, not a contract.)

## Principle: expose what the browser has; virtualize only what it lacks

The earlier draft of this RFC proposed virtualizing all four capabilities to keep
example output deterministic. That was the wrong driver: **book examples do not
need to be deterministic.** Determinism is a property the *test harness* wants for
pinned goldens — it is not a reason to reshape a runtime capability. Dropping it
collapses the design to a single, honest question per capability: **does the
browser actually have this?**

- **`Clock` → real browser time.** `clock.now()` returns the real wall clock via
  the page (`Date.now()`), and `clock.now_monotonic()` a nanosecond monotonic
  count (`performance.now()`). Output varies between runs; that is correct and
  fine. No virtualization.
- **`Net` → DEFERRED (see "Net is a later phase").** The browser *has* networking,
  but not in a shape the current ABI can consume synchronously, and `fetch` is not
  a socket. Backing it honestly is a real ABI project, so this phase leaves `Net`
  denied by omission rather than faking it.
- **`Env` → empty by default, virtualizable.** The browser has no process
  environment, so like `Dir` this has no real backing — but unlike `Dir` there's
  little a web example needs from it, so `env.get_env(k)` returning `None` is a
  fine default rather than a necessity. A page-supplied `{k: v}` map is a trivial
  extension if a specific example ever wants it; empty is the choice, not the
  truth.
- **`Dir` → in-memory scratch tree.** The browser has no filesystem, and unlike
  `Env`, an empty default is useless (a file example needs files), so this is the
  one capability that genuinely requires a backing. A per-run in-memory tree
  (seeded empty, or from a small page-supplied fixture) backs
  `read`/`write`/`subtree`/listing; `..` confinement is unchanged (pure logic).

The security guarantee is unchanged and, for Clock, is now simply the **browser's
own** guarantee: a page can already read the clock; a witchy cell doing so through
`Clock` is no more authority than the surrounding JavaScript already has. `Dir`
reaches only its in-memory tree — there is no real filesystem in the page to
reach — and its `..`/absolute confinement and entry policy (`dir.only`) are the
same logic the native host enforces. This does **not** relax the deny-by-omission
boundary for shipped Glamour apps, nor for this book's own cell: capability
support is an **explicit opt-in** an embedder must request family by family. A
page enables only what it hands over; the default host denies everything by
omission, exactly as before.

### Non-widening: the opt-in host is separate, structural, and per-family

Deny-by-omission is structural — witchyc tree-shakes imports, so a module that
reaches a capability imports a host function the JS object must provide, or
`WebAssembly.instantiate` throws a `LinkError`. The default `instantiate` provides
ONLY the pure/non-authority surface, so it denies every capability. The opt-in
host is reached by passing `instantiate(bytes, { capabilities })`; each requested
family (`clock` / `env` / `dir`) contributes its imports to a *superset* object,
and each is drift-checked against a frozen per-family catalog so it can never
silently widen beyond its declared surface. `Exec`, `Secret`, raw `Net`, the
top-level `mint_file` grant, argv and compiler introspection are simply never
built into that object — a module reaching them still fails to instantiate, opt-in
or not.

## Net is a later phase (the ABI prerequisite, stated precisely)

Backing `Net` in the browser is blocked on the host-call ABI, for two independent
reasons — either alone is sufficient:

1. **Synchronous vs. asynchronous.** Every Witchy capability host call is
   synchronous: the guest calls e.g. `net_recv_line_len(socket) -> i32` and
   consumes the length as the *inline* return before it continues. The browser's
   only network primitive, `fetch`, is asynchronous (a `Promise`). The sync
   escapes are unacceptable here: a blocking `XMLHttpRequest` freezes the page's
   main thread (forbidden — a teaching cell must not hang the tab), and a
   Worker + `SharedArrayBuffer` + `Atomics.wait` bridge is a re-architecture of
   how modules are instantiated and threaded (cross-origin-isolation headers, a
   worker transport, a blocking ring buffer) — out of scope for this phase and
   outside this host file.
2. **Socket vs. message.** `net.connect(addr) -> socket` is a raw TCP stream the
   guest drives byte-by-byte (`net_send_line` / `net_recv_line` — `std/http`
   writes HTTP/1.1 by hand over it). `fetch` exposes a whole request/response, not
   a byte socket, so it cannot back `net.connect` at all, even setting the
   sync/async problem aside.

Faking a synchronous socket, returning canned responses while calling it "real
Net," blocking the main thread, or silently changing `net.connect`'s semantics
would each break the parity guarantee (the compiled path must match the
interpreter, and both must match this host). So `Net` stays **denied by omission**
until the prerequisite exists:

> **ABI prerequisite for browser `Net`:** an async-suspending host-call mechanism
> (WebAssembly **JSPI**, or an **Asyncify** transform, or a Worker + SAB +
> `Atomics.wait` blocking bridge) **AND** a socket-shaped browser transport — or,
> alternatively, a new *http-level* capability the guest can call synchronously
> and the host can satisfy from a worker. Either is a standalone project with its
> own RFC; this one does not attempt it.

## Phases

- **Phase 1 (this RFC, implemented):** the opt-in `Clock` / `Env` / `Dir` host,
  the `browser_runnable` classifier field, the Node/Rust end-to-end tests, and
  the frontend-chapter/docs-cell prose. `Net` denied; `Exec`/`Secret` denied.
  Independently complete and shippable.
- **Phase 1.1 (deferred, harness only):** pin `output` goldens for the
  *deterministic* browser-runnable subset (`Dir` seeded from a fixed fixture,
  `Env` returning `None`). This needs a fixture-granting entry in the interpreter
  run path (an empty in-memory `Dir` + empty `Env` oracle) so the manifest can run
  those blocks deterministically; see "Consequences for the harness" below.
- **Phase 2 (deferred, ABI):** browser `Net`, once the prerequisite above lands.

## Consequences for the harness

The runnable-book classifier and `book/examples.json` record a pinned `output` per
`runnable` block (the interpreter-oracle output) and assert it downstream
(`scripts/validate_book_examples.mjs`). That golden is produced by running the
block through the interpreter, which grants `main` a **real cwd-backed `Dir`** and
reads the **real process `Env`** — so a `Dir`/`Env` block cannot be pinned
deterministically without a fixture-granting oracle, and a `Clock` block is
nondeterministic by nature.

Phase 1 therefore adds a **separate** `browser_runnable` field rather than
widening `runnable`: `runnable` stays Console-only and output-pinned (every
existing gate is untouched and green), while `browser_runnable` marks the superset
a `Clock`/`Env`/`Dir` cell can run under the opt-in host — with **no** pinned
`output`. This is deliberately the smaller, honest step: it does not fabricate a
golden for a nondeterministic run, and it does not flip the book's own
capability-denied cell into a false Run button. Phase 1.1 can later pin goldens
for the deterministic subset once the interpreter run path can grant an empty
in-memory `Dir` + empty `Env`.

This is a harness-classification change, not a runtime one, and it is the reason
the earlier draft over-reached: it tried to make the runtime deterministic to
satisfy the harness, when the harness can simply not pin output for the cells
that are legitimately nondeterministic.

## Scope / non-goals

- **`Net` stays un-runnable in phase 1** — the ABI prerequisite above is a
  standalone project. **`Exec` and `Secret` stay un-runnable** always: a native
  subprocess has no browser analogue, and host key material must not be simulated
  in a teaching context. Those blocks remain static; the frontend chapter notes
  why.
- **The book's own cell is unchanged.** It runs under the capability-denied
  `web/witchy-host.js`, so capability examples stay read-only *in this book*. The
  opt-in host is for an embedder (a teaching site / playground) that explicitly
  wires it in; wiring the docs cell onto it is a follow-up, not part of phase 1
  (doing it now would create Run buttons the book's shipped engine can't honor).
- This RFC does not resolve RFC-0077's test-runner mock model. The overlap is
  historical: RFC-0077's removed `mock_dir` backend and this RFC's browser `Dir`
  wanted the same *shape* of in-memory filesystem. RFC-0105 now supplies the
  shared fixture contract and transcript semantics across that FFI boundary
  later cleanup.

## Acceptance

Phase 1 (this RFC):

- The opt-in host runs a block whose footprint's families ⊆
  `{Console, Clock, Env, Dir}`: `Clock` against real wall/monotonic time, `Env`
  against an empty-or-page-supplied immutable map, `Dir` against a confined
  per-run in-memory tree. Verified end-to-end
  ([`web/witchy-runtime/capability-host.test.mjs`](../web/witchy-runtime/capability-host.test.mjs), driven by [`tests/browser/shim.rs`](../tests/browser/shim.rs)):
  `Env`/`Dir` output is byte-identical to the native interpreter oracle.
- Non-widening is verified: the **default** host still `LinkError`s on any
  `Dir`/`Clock`/`Env` program, and `Exec`/`Secret`/`Net` stay denied **even under
  the opt-in host** (their imports are never built).
- Confinement is verified on both backends: a `..`/absolute path is refused by the
  in-memory `Dir` and by the native run; `dir.only(Dir.ext(...))` denies a
  non-matching file. The in-memory tree never touches a real filesystem.
- `book/examples.json` gains a `browser_runnable` field (a superset of `runnable`,
  no pinned `output`); the existing `runnable`/`output` goldens and every gate
  reading them are unchanged.
- No new host authority is fabricated: `Clock` uses the browser's own clock, `Env`
  is empty/page-supplied, and `Dir` reaches only its in-memory tree — never a real
  filesystem (there is none in the page).

Later phases: pin goldens for the deterministic subset (1.1); browser `Net` once
the ABI prerequisite lands (2).
