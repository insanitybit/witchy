---
rfc: 0018
title: Compiler architecture — a workspace of stage-aligned crates
status: proposed
created: 2026-06-27
superseded-by:
tracking:
---

# RFC-0018: Compiler architecture — a workspace of stage-aligned crates

## Summary

Split witchy's single crate into a **Cargo workspace of focused, stage-aligned
crates**, one per pipeline stage that `spec/architecture.md` already names. This
forces the `codegen.rs` monolith apart, turns the (already clean) stage seams
into **compiler-enforced** boundaries, and makes each stage independently
testable and documented — without changing what the compiler does. The
interpreter stays its own crate: it is the independent reference oracle, and
keeping it a separate implementation is the point.

## Motivation

witchy's file-map is clean — `spec/architecture.md` gives each `src/` file one
job — but a **single crate cannot enforce those seams**, and the layout has two
concrete costs:

- **The lowering monolith.** `codegen.rs` is ~8.6k lines: AST→WIR lowering, the
  in-place + inline fast paths, capability host-import emission, per-shape
  equality/format helpers — all in one file. The file has one *role* (lower the
  checked AST), but it is far too big to navigate, and nothing structurally forces
  it to decompose. `example_tests.rs` (~15k) is the same shape on the test side.
- **Conventional, not enforced, boundaries.** Because everything is one crate, any
  module can reach into any other's internals. The discipline that keeps stages
  separate is convention + review, not the compiler.

`rustc` (a workspace split by compiler phase) and `ripgrep` (a workspace of
focused crates behind a thin binary, with standalone reusable pieces like
`ignore`/`globset`) both show the fix: crates as the unit of enforced boundary.

## Design

Reorganize `src/` into a workspace whose crates mirror the existing file-map:

| crate | absorbs (today's files) | role |
|---|---|---|
| `witchy-syntax` | lexer, parser, ast, format | source → AST, plus the canonical formatter |
| `witchy-types` | typeck, traits | annotation + HM checking; trait desugaring + monomorphization |
| `witchy-wir` | wir, wir_opt, wir_prelude, wir_encode | the WIR types, the peephole opt, the runtime-helper prelude, and the WIR→wasm encoder |
| `witchy-lower` | the AST→WIR half of `codegen.rs` | **the lowering, extracted — the move that finally breaks the monolith** |
| `witchy-runtime` | runtime, confine | the wasmtime sandbox + capability host functions (the TCB) |
| `witchy-interp` | interpreter, comptime | the reference **oracle** (parity diff), `comptime` evaluation, build steps |
| `witchy-caps` | capabilities, grants | footprint analysis (`witchy caps`/`caps-diff`), confinement, grant documents |
| `witchy` | main | the thin CLI binary |

The single run path is `witchy-lower` → `witchy-wir` (encode) → `witchy-runtime`
(execute under wasmtime). `witchy-interp` is the separate oracle the differential
tests run alongside it. The carve-up above is the starting decomposition; crates
may split further as they grow (e.g. `witchy-lower` into expr-lowering vs the
in-place/inline fast paths).

**What it buys:**

- **The monolith decomposes by construction.** `codegen.rs`'s lowering lands in
  `witchy-lower` and its WIR encoder in `witchy-wir` — they can no longer share a
  file because they're in different crates.
- **Stage seams become compile errors.** Crate privacy means a pass cannot reach
  into another stage's internals; the boundary the file-map *describes* is now
  *enforced*.
- **Independent testing + docs.** Each crate gets its own test target and rustdoc;
  the 15k `example_tests.rs` can be split to live beside the crates it exercises.
- **Faster incremental builds.** Touching the lowering rebuilds `witchy-lower`,
  not the world.
- **The thin binary stays thin** (`main.rs` is already ~2k); the logic is
  libraries, as in ripgrep's `core` + library crates.
- **The oracle stays independent.** `witchy-interp` is its own crate executing the
  AST directly — *not* merged with the compiled path — so it remains an
  independent implementation, which is exactly what lets the parity diff catch
  lowering bugs.

## Migration

Mechanical and parity-preserving; the standing discipline (keep green,
differential tests as the net) holds throughout. Extract one crate at a time,
leaf stages first, so each step is a pure move with no behavior change:

1. **Leaf, dependency-free stages first** — `witchy-syntax`, then `witchy-caps`,
   `witchy-wir` (the IR has few inbound deps). Each: move files, declare the
   crate, fix `use` paths, run the suite.
2. **`witchy-lower` — extract the AST→WIR lowering out of `codegen.rs`.** The
   headline step; do it once `witchy-wir` and `witchy-types` are crates so its
   dependencies are already drawn.
3. **`witchy-interp`, `witchy-runtime`** — the two executors, against the now-stable
   `witchy-wir`/`witchy-lower` boundary.
4. **`witchy` binary last** — it depends on everything; it shrinks to wiring.

No phase changes observable behavior; each is green before the next.

## Acceptance

- `codegen.rs` no longer exists as a monolith — it is `witchy-lower` (lowering)
  plus the already-separate WIR encoder in `witchy-wir`.
- Reaching across a stage boundary is a compile error, not a review comment.
- Each stage builds, tests, and documents independently; the CLI binary is wiring.

## Prior art

- **rustc** — a Cargo workspace split by compiler phase (`rustc_parse`,
  `rustc_hir`, `rustc_typeck`, `rustc_codegen_ssa`, …) with a thin
  `rustc_driver`. This RFC is the same shape at witchy's scale.
- **ripgrep** — a workspace of focused crates (`grep`, `searcher`, `printer`,
  `matcher`, …) behind a thin `core` binary, with the most reusable pieces
  (`ignore`, `globset`) standing alone. witchy's `witchy-syntax`/`witchy-wir` are
  similar candidates for clean, independently-useful crates.
