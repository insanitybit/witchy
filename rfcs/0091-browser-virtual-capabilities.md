---
rfc: 0091
title: "Browser-runnable capability examples — real Clock/Net/Env, an in-memory Dir"
status: proposed
created: 2026-07-10
supersedes:
superseded-by:
tracking: >
  Proposed. Unblocks the runnable-book gap: today only Console-only examples run
  in the browser (106 of 152 book blocks); the other 46 are blocked because their
  authority imports link as trapping stubs. 36 of the 38 with a real footprint use
  ONLY Dir/Net/Clock/Env. Back those stubs with what the browser actually has —
  real time, real fetch, a default-empty environment — and an in-memory Dir where
  the browser has nothing. Book examples do NOT need to be deterministic;
  Clock/Net are exposed as-is, Env defaults to empty (a page-supplied map if ever
  needed), and only Dir requires a real in-memory backing.
---

# RFC-0091: Browser-runnable capability examples

## Problem

The docs app (this book) turns fenced `witchy` blocks into editable, executable
cells that compile to WebAssembly and run on the browser's own engine. A cell is
made runnable only when its program is **`Console`-only** (`example_tests.rs`:
`console_only = footprint.total.keys().all(|k| k == "Console")`); everything else
is served as static, un-runnable code because `compile_source` links every
authority import as a **trapping stub** ("the browser has none", `src/lib.rs`).

Measured against `book/examples.json` (152 blocks): **106 run**, **46 do not**.
Of the 46, the non-Console footprints are `Dir` 21, `Net` 9, `Clock` 3, `Env` 3,
`Secret`/`SecretStore` 1 each, `Exec` 1 — and **36 of the 38 with a real
footprint use ONLY `Dir`/`Net`/`Clock`/`Env`**. So a reader learning file I/O,
HTTP, or time can read the code but never press Run. This RFC makes those 36
runnable (106/152 → ~142/152), leaving only `Exec` and `Secret` un-runnable.

## Principle: expose what the browser has; virtualize only what it lacks

The earlier draft of this RFC proposed virtualizing all four capabilities to keep
example output deterministic. That was the wrong driver: **book examples do not
need to be deterministic.** Determinism is a property the *test harness* wants for
pinned goldens — it is not a reason to reshape a runtime capability. Dropping it
collapses the design to a single, honest question per capability: **does the
browser actually have this?**

- **`Clock` → real browser time.** `clock.now()` returns the real wall clock via
  the page. Output varies between runs; that is correct and fine. No virtualization.
- **`Net` → real `fetch`.** `http.get`/`http.post` issue a real request through
  the browser, subject to the browser's own CORS/mixed-content rules (the host is
  not adding or removing reach — the browser's same-origin policy is the boundary,
  as it is for any web page). No scripting, no canned responses.
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

The security guarantee is unchanged and, for Net/Clock, is now simply the
**browser's own** guarantee: a page can already read the clock and `fetch` within
CORS; a witchy cell doing so through these capabilities is no more authority than
the surrounding JavaScript already has. `Dir` reaches only its in-memory tree —
there is no real filesystem in the page to reach. This does **not** relax the
deny-by-omission boundary for shipped Glamour apps: a real app on a real browser
host still decides its own capability posture; this RFC is about what the
docs/playground host offers a teaching cell.

## Consequences for the harness (not a blocker)

The runnable-book classifier and `book/examples.json` currently record a pinned
`output` per runnable block and assert it. A `Clock`- or `Net`-backed example is
nondeterministic, so it cannot carry a pinned golden. Options, to settle in
review:

- Classify such blocks as **runnable but output-unpinned** — the cell runs and
  shows whatever it produces; the manifest records `runnable: true` with no
  `output` assertion. The native differential harness still checks that the
  program *compiles and runs* on both backends (it just doesn't diff stdout for
  a clock/net example — the same latitude the suite already needs for anything
  time- or network-dependent).
- Keep pinned goldens only for the deterministic subset (`Dir` with a fixed
  fixture, `Env` returning `None`).

This is a harness-classification change, not a runtime one, and it is the reason
the earlier draft over-reached: it tried to make the runtime deterministic to
satisfy the harness, when the harness can simply not pin output for the cells
that are legitimately nondeterministic.

## Scope / non-goals

- **`Exec` and `Secret` stay un-runnable in the browser.** A native subprocess
  has no browser analogue, and host key material must not be simulated in a
  teaching context. Those 2 blocks remain static; the book notes them.
- This RFC does not resolve RFC-0077's test-runner mock model. The overlap is
  narrow: 0077's `mock_dir` in-memory backend and this RFC's browser `Dir` want
  the same in-memory filesystem implementation, so build it once and consume it
  from both hosts. Clock/Net need no mock here — the browser has the real thing;
  Env defaults to empty and would reuse the same page-supplied-map plumbing only
  if an example ever needs a value.

## Acceptance

- A book block whose footprint ⊆ `{Console, Clock, Net, Env}` is runnable in the
  browser against the real browser clock / fetch / (empty) env; a block adding
  `Dir` is runnable against an in-memory tree.
- `book/examples.json` supports a runnable block with no pinned `output` (the
  nondeterministic case); deterministic blocks may still pin output.
- Browser-runnable count rises from 106 toward ~142; the remaining un-runnable
  blocks are exactly the `Exec`/`Secret` set, which the frontend chapter notes.
- No new host authority is fabricated: Net/Clock use the browser's own APIs
  (CORS/same-origin unchanged), Env is empty, and `Dir` reaches only its
  in-memory tree — never a real filesystem (there is none in the page).
