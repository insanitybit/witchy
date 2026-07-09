---
rfc: 0072
title: "Diagnostic goldens: lock the error surface, then polish it"
status: implemented
created: 2026-07-07
tracking: quality audit 2026-07-07 (scratch/audit-2026-07-07-quality/REPORT.md, F3/F4/F9);
  Phase 1 harness ef9c99a1 (2026-07-08); phase 2 shipped 2026-07-09 —
  file context on type errors (8da0cf79), backtrace-switch trailer on traps,
  arity errors show parameter types (7d3465f4), jargon-class messages guided
  (d7a37976) + goldens (cbb840fd). The known-bad line-zero position (BUG-162)
  is the documented residual, tracked separately.
related:
  - "0045 (compiled trap diagnostics — the runtime-error format this locks)"
  - "0050 (method-call generalization — Part 2 error contract, BUG-303, lands under this harness)"
  - "0054 (structured errors, in-progress — goldens are the regression net its migration needs)"
  - "0070 D2/D3 (one checker / error cut — both reshape messages; goldens gate the reshaping)"
---

# RFC-0072: Diagnostic goldens — lock the error surface, then polish it

## Summary

witchy's diagnostics are better than the codebase can prove: live probes show
consistent voice, positions, and actionable hints — but **no test asserts any
message verbatim**. Every error assertion in the suite is a loose
`.contains(...)`. This RFC adds a golden (snapshot) test harness over the full
diagnostic surface — parse, layout, link, type, capability, lowering-reject,
runtime trap — and then lands a small, specific polish pass on the gaps the
audit probes found, *under* that harness. Goldens first, polish second: the
harness is what makes RFC-0054's coming migration and RFC-0070's D2/D3
reshaping safe.

## Motivation

Probed live at `ffca3bf`:

```
type error: `main`, line 5: `x` is declared `Int` but the value disagrees: expected `Int`, found `String`
type error: `main`, line 5: `wq_probe_arity.add` expects 2 argument(s) but got 3
type error: `main`, line 2: call to unknown function `mystery`
type error: `json.stringify` looks like a module-qualified call, but `json` is not imported — add `import json`
link error: unknown type `Wibble` (in module `wq_probe_unktype`) — … Qualify it (`module.Wibble`) or add `from module import Wibble`
cannot read `./jsn.witchy`: No such file or directory (os error 2) — did you mean `import json`?
runtime error: list index 9 out of bounds (length 2)
```

This is a *good* error surface — consistent lowercase voice, expected/found
convention, did-you-mean hints for functions, types, and modules. Three
problems:

1. **Nothing locks it.** `src/example_tests.rs` asserts only substrings
   (e.g. `err.contains("ownership convention")`). Any of these messages can
   silently degrade — drop its hint, lose its position — and the suite stays
   green. A prior audit's reviewer flatly (and wrongly) claimed "type errors
   have no line numbers"; there is no artifact that documents or defends what
   the messages actually are.
2. **Churn is scheduled.** RFC-0054 (structured errors, in-progress) and
   RFC-0070's D2 (backend rejections become check-time diagnostics) and D3
   (error cut) will each rewrite message plumbing. Without goldens, each
   migration step can only be eyeballed.
3. **The known gaps have nowhere to land as tests.** BUG-303 (RFC-0050 Part 2
   error contract unmet), BUG-307 (mono diagnostics mask checker errors),
   BUG-341 (comptime line numbers point past EOF), BUG-162 (LSP line zero),
   BUG-107 (trap source-location leniency) are all *message-quality* bugs whose
   fixes need exactly this harness to stay fixed.

Small text gaps found by probing, in scope for the polish pass:

- the **import-hint** type error is the only probed message with **no
  position** (`type error: \`json.stringify\` looks like…` — no `` `func`,
  line N ``);
- **no filename** anywhere in type/link errors — ambiguous the moment a
  project has two modules (multi-file is the norm in `projects/`);
- arity errors say `expects 2 argument(s) but got 3` without showing the
  callee's signature, which the checker has;
- runtime traps carry no source position (tracked: BUG-107 / RFC-0045
  residual — **not re-decided here**) and never mention
  `WITCHY_WASM_BACKTRACE=1`, which exists precisely for this moment;
- internal jargon leaks in rarer paths (mono/type-variable phrasing) —
  inventory during golden authoring, fix the worst.

## Design

### 1. The harness

A new dev-dependency on `insta` (or a hand-rolled goldens directory if the
no-new-deps preference wins — decided at implementation; `insta` is the
recommendation for its review workflow) and a new test file
`src/diagnostic_golden_tests.rs`:

- **One golden per diagnostic class**, ~40–60 cases total, each a minimal
  `.witchy` program compiled/run in-process, capturing the **full stderr
  text**: lex, layout/indentation, parse (incl. unclosed-block EOF), link
  (unknown module / unknown type / privacy), import hints (function, type,
  module misspelling), typeck (mismatch, arity, unknown fn, ownership
  convention, capability misuse, grantable violations), lowering
  `reject_reason`, and runtime traps on **both backends** (the RFC-0045
  template set: OOB, parse-int, NaN order, fail, secret-required…).
- **Parity rule:** every runtime-trap golden runs interpreter and compiled
  backends and snapshots both outputs — a golden *pair* diverging is a parity
  failure caught at the message level (complements the existing differential
  suite; RFC-0045's lenient slice means the pair may legitimately differ
  today — the golden records the *current* accepted difference so any drift is
  loud, and BUG-107's eventual fix shows up as a deliberate golden update).
- Goldens live in `src/snapshots/` (insta default) and are reviewed like code:
  a message change is a visible diff in the PR, per the freeze-don't-drift
  spirit.
- CI: no new workflow — `cargo nextest run --workspace` picks the tests up;
  `insta` fails on pending snapshots.

### 2. The polish pass (each item lands as a golden diff)

1. **Filename in link/type errors**: prefix becomes
   `` type error: pm.witchy: `main`, line 5: … `` (linker already knows the
   module→file map; thread it into `at_loc` /
   `crates/witchy-types/src/typeck.rs:351`).
2. **Position on the import-hint error** (the one probed stray).
3. **Arity errors show the signature**:
   `` `add` expects 2 arguments (`fn add(a: Int, b: Int) -> Int`) but got 3 ``
   — the checker's fn table has the signature at the error site.
4. **Traps advertise the backtrace switch**: append
   `(re-run with WITCHY_WASM_BACKTRACE=1 for a backtrace)` to compiled trap
   output when the var is unset — one change in the runtime's trap formatting,
   goldened on both backends.
5. **Jargon sweep**: inventory `mono` / `Var(n)` / WIR-speak reachable from
   user programs while writing goldens; rewrite the reachable ones in user
   terms.

Explicitly **not** in scope: structured/JSON error output (RFC-0054 owns it),
trap source positions (BUG-107/RFC-0045 residual), column numbers and
span-carrying error types (RFC-0054's structured errors are where spans belong
— retrofitting columns into prose strings twice is waste).

### Verification

- `./scripts/check.sh --fast` while iterating; full gate via
  `./scripts/merge-queue.sh submit <branch>`.
- Landmines: never `cargo fmt` (hand-formatted Rust); `spec/stdlib.md` is
  generated — untouched here; parity is the prime directive — every polish
  item that touches runtime messages gets goldens on **both** backends, and
  any intentional difference is visible in the golden pair.

## Alternatives

- **Wait for RFC-0054 and golden the structured output instead** — rejected:
  0054 is in-progress with an unbounded horizon, and its migration is exactly
  what needs a net *under* it. Goldens over today's prose are the cheapest net
  and remain useful after (the human-rendered form still needs locking).
- **Hand-rolled golden files instead of insta** — acceptable; costs the
  review/update workflow. Decided at implementation.
- **Assert full messages inline in example_tests.rs** — rejected: inline
  string literals rot into `.contains` again under update pressure; a snapshot
  workflow makes updating deliberate and reviewable.

## Drawbacks

- Golden churn: legitimate message improvements now touch snapshot files.
  That is the point — the churn becomes visible and reviewed.
- ~40–60 tiny compile-and-run tests add suite time; they are in-process and
  trivially parallel under nextest.
- A new dev-dependency (if insta): pinned latest per repo policy.

## Prior art

- rustc's `ui` test suite (`.stderr` goldens) — the reference model: message
  quality is maintainable *only* because every diagnostic is a tracked file.
- `insta` is the de-facto Rust snapshot harness (used by ruff, biome).
- This repo's own executed-```witchy-fence discipline is the same idea aimed
  at docs; this RFC aims it at stderr.
