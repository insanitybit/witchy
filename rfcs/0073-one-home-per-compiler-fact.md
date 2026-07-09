---
rfc: 0073
title: "One home per compiler fact: shared layout constants, deduped type parsing, and the new-semantic checklist"
status: implemented
created: 2026-07-07
tracking: quality audit 2026-07-07 (scratch/audit-2026-07-07-quality/REPORT.md, F6/F7/F12);
  shipped 2026-07-09 — typeck `named_builtin` single table + rights-parser dedup
  and analysis.rs shape-matcher tests (dd104f16); shared `witchy_wir::layout`
  owning HEAP_REDZONE/RC_SIZE_MASK/type_tag_of with vectors, runtime+lower
  importing it (af04a6d0); spec/value-model.md + CONTRIBUTING adding-a-semantic
  checklist (85ce4b13 + the spec-polish merge).
related:
  - "0018 (workspace split — created the crate boundaries these facts now straddle)"
  - "0037 (type-confusion sanitizer — type_tag_of, currently codegen-only)"
  - "0051 (memory-safety invariants — the discipline this mechanizes)"
  - "0058 (differential-harness integrity — the runtime net; this is the compile-time net)"
---

# RFC-0073: One home per compiler fact

## Summary

The twin-backend discipline ("zero silent divergence") is currently enforced by
comments and tribal knowledge in three places where it could be enforced by the
compiler itself:

1. **Layout constants are duplicated across crates and synced by prose.**
   `HEAP_REDZONE` is defined twice — `witchy-wir/src/wir_helpers/mod.rs:156`
   and `witchy-runtime/src/runtime.rs:271` — with a doc-comment saying "MUST
   equal `witchy_runtime`'s". Nothing checks it.
2. **The frontend's type-name table is written twice.** `Checker::to_ty`
   (`typeck.rs:2105`) and `Checker::to_ty_generic` (`typeck.rs:2174`) carry
   near-identical 30-arm builtin-name matches (plus three copy-pasted rights
   parsers, `typeck.rs:51-230`); adding a builtin type means editing twin
   matches in lockstep.
3. **"What must change when a semantic lands" is undocumented**, and the
   ownership analysis' shape matchers — the subsystem where a quiet regression
   is a UAF-class bug — are tested only end-to-end.

This RFC gives each fact one home: a shared layout module with cross-crate
const-asserts, a single type-name table both `to_ty` variants consume, unit
tests for `analysis.rs`'s `self_*` matchers, and a short "adding a semantic"
checklist in CONTRIBUTING.md. No behavior changes; every step is verifiable by
the existing gate.

## Motivation

Parity is the prime directive, and the repo already pays for it properly at
the *output* level (differential suite, parity sweep, RFC-0058). But the
*input* level — the constants and tables both backends assemble from — has no
mechanical guard:

- If a refactor bumps `HEAP_REDZONE` in `witchy-runtime` but not
  `witchy-wir`, heap poisoning and allocation math disagree silently: the
  exact class of bug the redzone exists to catch.
- `type_tag_of` (`witchy-lower/src/codegen/mod.rs:130`, FNV-1a with inline
  magic constants) computes the RFC-0037 type tag **only in codegen** — the
  interpreter has no dual and no shared reference. Any future
  interpreter-side tag check must independently re-derive the same hash, the
  textbook setup for silent divergence.
- The value-model facts (8-byte slots; string `[i32 len][utf8]`; RC cell
  `size|tag @ -4, rc @ -8`; tuple `4 + 8n`) are scattered across
  `codegen/mod.rs:8-12` and `wir_helpers` comments; no single document maps
  the interpreter's `Value` enum to the compiled representation.
- `to_ty` vs `to_ty_generic`: the audit confirmed the twin matches are ~95%
  identical, differing only in the unknown-name fallback (fresh var vs
  generic-param lookup). Every new capability or builtin type is a two-site
  edit today; a missed site is a wrong-but-plausible type judgment.
- `analysis.rs`'s six `self_*` shape matchers and the `Facts`
  identity-keying contract (`analysis.rs:44-62, 117-200`) power every
  in-place optimization. Their soundness history is exactly why this matters
  (SEC-037/SEC-039); they deserve direct unit tests, not only
  end-to-end coverage.

None of these is a bug today. All of them are the *mechanism* by which the
next bug arrives.

## Design

### 1. `layout` — one module, consumed by all three crates

A small `layout` module in `witchy-wir` (the lowest crate all three already
depend on; a new leaf crate is overkill for ~10 constants), re-exported where
needed:

```rust
pub const DATA_BASE: u32 = 8;
pub const HEAP_REDZONE: usize = 8;
pub const RC_SIZE_MASK: i32 = 0x00FF_FFFF;
pub const STRING_HEADER: u32 = 4;   // [i32 len] before utf8 bytes
pub const SLOT_SIZE: u32 = 8;       // i64/f64 value slots
// … tuple header, page size, rc-cell offsets
```

`witchy-runtime` and `witchy-lower` delete their local copies and import.
Where a crate genuinely cannot depend on `witchy-wir`, it keeps a local const
plus a `#[test]` const-equality assertion against the shared one (the test
crate can see both). `type_tag_of` moves here too, with its FNV-1a constants
named and a doc-comment stating the contract (range `1..=255`, embedded in
ctor-header high bits) plus fixed test vectors.

### 2. `spec/value-model.md` — the map between the backends

One page: a table from each `interpreter.rs` `Value` variant to its compiled
representation, the rc-cell diagram, and pointers into the code. Linked from
`CONTRIBUTING.md` and from `codegen/mod.rs`'s header comment. This is the
document the audit's backend reviewer had to reverse-engineer from two crates.

### 3. Dedupe the frontend type table

Extract the shared 30-arm match into one function; the two callers pass their
fallback:

```rust
fn named_to_builtin(&mut self, name: &str, args: &[ast::Type],
                    on_unknown: impl FnOnce(&mut Self) -> Ty) -> Ty
```

`to_ty` passes `|c| c.fresh()`; `to_ty_generic` passes the `vars`-keyed
lookup. Likewise the three rights parsers (`dir_rights`/`file_rights`/
`net_rights`) collapse to one accumulating helper parameterized by the
right-name mapping. Pure refactor; the type-checker's observable judgments are
unchanged and the whole workspace suite is the oracle.

### 4. Unit tests for the ownership oracle

A `#[cfg(test)] mod shape_matcher_tests` in `analysis.rs`: for each `self_*`
matcher, one accepting case and two rejecting near-misses (wrong variable,
wrong callee, shadowed name), plus a test documenting the `Facts`
identity-keying invariant (facts keyed by statement pointer are valid only for
the analyzed AST instance). These make the RFC-0051 family's *contract*
executable without touching its behavior. (The family stays closed to
extension per RFC-0051 and the standing no-new-fast-paths rule; a fence
comment at the `*_cap` helper block in `wir_helpers` cross-references
RFC-0051 and `analysis.rs` so the rationale lives where the code is.)

### 5. The checklist

A short "Adding a semantic" section in `CONTRIBUTING.md`: for a new binary
op / value type / builtin / trap, the ordered list of files that must change
(ast → typeck (`named_to_builtin`) → interpreter eval → codegen lower →
wir_helpers → diag template → differential test → book example). Ten lines;
today it is tribal.

### Verification

- Each step is independently gated by `./scripts/check.sh --fast`; full gate
  via `./scripts/merge-queue.sh submit <branch>`.
- The refactor steps (1, 3) must produce **zero** golden/differential churn —
  any test diff is a stop-and-investigate signal, not a snapshot update.
- Landmines: never `cargo fmt` (hand-formatted Rust — new code matches the
  local style by hand); `spec/stdlib.md` is generated (untouched);
  no new `*_cap`/`self_*` entries — this RFC documents and tests the family,
  it does not extend it.

## Alternatives

- **A new `witchy-layout` leaf crate** instead of a module in `witchy-wir` —
  cleaner dependency story, one more crate to version; rejected for now,
  trivial to promote later if a fourth consumer appears.
- **Do nothing / comments are enough** — rejected: the `HEAP_REDZONE` comment
  is the counterexample; a MUST in prose with no assert is a latent parity
  bug.
- **Full visitor/table-driven rewrite of `to_ty`** — rejected as
  over-engineering; the closure-parameterized extraction removes the
  duplication without inventing machinery.

## Drawbacks

- `witchy-runtime` gains a dependency edge on `witchy-wir` (or a const-assert
  test) — mild coupling, already implied by the shared ABI.
- Moving `type_tag_of` touches a security-adjacent hash: the fixed test
  vectors and untouched call sites are the guard; any vector change is a
  loud, reviewed event.
- The checklist can rot like any doc; it is one screen and lives next to the
  parity rules people already read.

## Prior art

- The repo's own `wir_helpers` doc-comment ("MUST equal…") is this RFC's
  motivation written by its author — the intent exists, only the enforcement
  is missing.
- rustc's `rustc_abi` crate: layout facts in one crate consumed by codegen
  and const-eval alike.
- RFC-0058 did the same move for the differential harness: turn a discipline
  into a mechanism.
