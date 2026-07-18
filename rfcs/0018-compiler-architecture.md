---
rfc: 0018
title: Compiler architecture — a workspace of stage-aligned crates
status: implemented
created: 2026-06-27
superseded-by:
tracking:
---

# RFC-0018: Compiler architecture — a workspace of stage-aligned crates

The shipped workspace boundary is declared in [`Cargo.toml`](../Cargo.toml) and
documented in [`spec/architecture.md`](../spec/architecture.md), with each stage
crate exposed through the `crates/` workspace members.

> **Implemented.** The workspace is the seven library crates `witchy-syntax`,
> `witchy-types`, `witchy-wir`, `witchy-lower`, `witchy-runtime`, `witchy-interp`,
> and `witchy-caps` under `crates/`, plus the `witchy` binary package (the CLI +
> the wasm-playground cdylib + the LSP/PM/idp tooling). The two-part migration
> went as planned: the SCC was broken by four surgical helper-relocations plus the
> linker→comptime/tagged callback inversion, then the crates were extracted
> bottom-up. Leaf placement (left open below) resolved as: the AST-level base
> passes (`aliases`/`consts`/`fmt`/`async_lower`/`generators`/`optimize`/`reflect`/
> `format`/`derive`/`records`/`doc`/`linker`) fold into `witchy-syntax`;
> `value`/`net`/`native`/`confine` (wasm-safe) + the native-gated wasmtime sandbox
> into `witchy-runtime`; `codegen`+`analysis` into `witchy-lower`. The residual
> `traits↔typeck` and `interpreter↔comptime↔tagged↔pipeline` cycles live inside a
> single crate each, which is fine. `idp` (OIDC/JWT) stayed binary-only.

## Summary

Split witchy's single crate into a **Cargo workspace of focused, stage-aligned
crates**, one per pipeline stage that [`spec/architecture.md`](../spec/architecture.md) already names. This
forces the `codegen.rs` monolith apart, turns the (already clean) stage seams
into **compiler-enforced** boundaries, and makes each stage independently
testable and documented — without changing what the compiler does. The
interpreter stays its own crate: it is the independent reference oracle, and
keeping it a separate implementation is the point.

## Motivation

witchy's file-map is clean — [`spec/architecture.md`](../spec/architecture.md) gives each `src/` file one
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

## Dependency reality (measured)

The file-map's *stage* seams are clean, but the *module* dependency graph
(measured from real `crate::` references, doc-comment links excluded) is **not a
DAG** — and Rust crates cannot form a cycle. Two facts shape the plan:

- **A clean upstream DAG** is already crate-able as-is: `ast`, `lexer`, `parser`,
  `value`, `confine`, `fmt`, `net`, `consts`, `aliases`, `async_lower`,
  `generators`, `optimize`, `analysis`, `idp`, and the whole `wir` group
  (`wir`/`wir_encode`/`wir_opt`/`wir_prelude`, self-contained). These extract with
  no cycle-breaking.
- **A 13-module strongly-connected component** — `typeck, codegen, format,
  linker, comptime, tagged, interpreter, records, derive, traits, capabilities,
  native, doc` — is mutually dependent and so **cannot be split into the
  `witchy-types`/`witchy-lower`/`witchy-interp`/`witchy-caps` crates until its
  cycles are cut.** The cuts, easiest first:
  - `typeck → codegen`: a single call (`codegen::lambda_outer_assigns`) — move it.
  - `typeck ↔ format`, `records ↔ derive`, `traits ↔ typeck`: small, local.
  - **The hard one — `linker ↔ comptime`/`tagged ↔ interpreter`:** genuine
    compile-time-evaluation co-recursion (`comptime`/tagged literals run the
    interpreter; the interpreter links std to run them; linking drives
    comptime/tagged expansion). Cut by **dependency inversion** — the linker takes
    a comptime-evaluator callback rather than calling `comptime` directly, and the
    interpreter receives an *already-linked* module rather than calling `linker`.

The split is therefore a *two-part* effort, not a file-move: (1) extract the
clean upstream crates; (2) break the SCC's cycles, which is the real
architectural work and the prerequisite for the four middle/back-end crates.

A second constraint the crates must honor: the lib is also a **`cdylib` built for
`wasm32`** (the browser playground, `native` feature off). Every crate on the
compile path (`syntax`, `types`, `wir`, `lower`) must stay wasm-clean; only
`witchy-runtime` (wasmtime) and the native-gated test/helpers are `native`-only.
Each new crate replicates the `native` feature gate accordingly.

## Migration

Parity-preserving throughout (keep green; differential tests as the net):

1. **Workspace skeleton + the clean upstream crates.** Extract `witchy-wir`
   (self-contained) first, then `witchy-syntax` (`ast`/`lexer`/`parser` + the
   ast-only passes). Pure moves + `use`-path rewrites + per-crate `native` gating;
   no behavior change.
2. **Break the SCC, easy cuts first** — move `lambda_outer_assigns`; resolve
   `typeck↔format`, `records↔derive`, `traits↔typeck`. Each cut is its own
   green commit.
3. **Invert the compile-time-eval cycle** (`linker`/`comptime`/`tagged`/
   `interpreter`) — the load-bearing refactor; once cut, `witchy-types`,
   `witchy-lower`, `witchy-interp`, and `witchy-caps` become separable.
4. **`witchy-runtime`** (wasmtime host, native-only) and the **`witchy` binary**
   last — the binary shrinks to wiring.

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
